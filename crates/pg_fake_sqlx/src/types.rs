use std::fmt::{self, Write};

use bigdecimal::BigDecimal;
use pg_fake::value::{BaseType, Value};
use sqlx::{
    Arguments, Decode, Encode, Type, TypeInfo,
    encode::IsNull,
    error::{BoxDynError, UnexpectedNullError},
};

use crate::{PgFake, PgFakeValueRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PgFakeTypeInfo {
    pub base: Option<BaseType>,
    pub typmod: i32,
}

impl PgFakeTypeInfo {
    pub fn new(base: BaseType) -> Self {
        Self {
            base: Some(base),
            typmod: -1,
        }
    }

    pub fn with_typmod(base: BaseType, typmod: i32) -> Self {
        Self {
            base: Some(base),
            typmod,
        }
    }
}

impl fmt::Display for PgFakeTypeInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

impl TypeInfo for PgFakeTypeInfo {
    fn is_null(&self) -> bool {
        self.base.is_none()
    }

    fn name(&self) -> &str {
        match self.base {
            Some(BaseType::Bool) => "BOOL",
            Some(BaseType::Int2) => "INT2",
            Some(BaseType::Int4) => "INT4",
            Some(BaseType::Int8) => "INT8",
            Some(BaseType::Float4) => "FLOAT4",
            Some(BaseType::Float8) => "FLOAT8",
            Some(BaseType::Numeric) => "NUMERIC",
            Some(BaseType::Text) => "TEXT",
            Some(BaseType::Varchar) => "VARCHAR",
            Some(BaseType::Bpchar) => "BPCHAR",
            Some(BaseType::Bytea) => "BYTEA",
            Some(BaseType::Uuid) => "UUID",
            Some(BaseType::Date) => "DATE",
            Some(BaseType::Time) => "TIME",
            Some(BaseType::Timestamp) => "TIMESTAMP",
            Some(BaseType::TimestampTz) => "TIMESTAMPTZ",
            Some(BaseType::Interval) => "INTERVAL",
            Some(BaseType::Json) => "JSON",
            Some(BaseType::Jsonb) => "JSONB",
            None => "NULL",
        }
    }

    fn type_compatible(&self, other: &Self) -> bool {
        match (self.base, other.base) {
            (None, _) | (_, None) => true,
            (Some(left), Some(right)) => {
                left == right
                    || matches!(
                        (left, right),
                        (
                            BaseType::Text | BaseType::Varchar | BaseType::Bpchar,
                            BaseType::Text | BaseType::Varchar | BaseType::Bpchar
                        )
                    )
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct PgFakeArguments {
    pub(crate) values: Vec<Value>,
    pub(crate) types: Vec<PgFakeTypeInfo>,
}

impl Arguments for PgFakeArguments {
    type Database = PgFake;

    fn reserve(&mut self, additional: usize, _size: usize) {
        self.values.reserve(additional);
        self.types.reserve(additional);
    }

    fn add<'t, T>(&mut self, value: T) -> Result<(), BoxDynError>
    where
        T: Encode<'t, Self::Database> + Type<Self::Database>,
    {
        let type_info = value.produces().unwrap_or_else(T::type_info);
        let previous_len = self.values.len();
        let is_null = value.encode(&mut self.values)?;
        if is_null.is_null() {
            assert_eq!(self.values.len(), previous_len);
            self.values.push(Value::Null);
        } else {
            assert_eq!(self.values.len(), previous_len + 1);
        }
        self.types.push(type_info);
        Ok(())
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn format_placeholder<W: Write>(&self, writer: &mut W) -> fmt::Result {
        write!(writer, "${}", self.values.len())
    }
}

sqlx_core::impl_into_arguments_for_arguments!(PgFakeArguments);
sqlx_core::impl_encode_for_option!(PgFake);

impl<T: ?Sized> Type<PgFake> for sqlx::types::Json<T> {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::Jsonb)
    }

    fn compatible(type_info: &PgFakeTypeInfo) -> bool {
        matches!(type_info.base, Some(BaseType::Json | BaseType::Jsonb))
    }
}

impl<'q, T: serde::Serialize> Encode<'q, PgFake> for sqlx::types::Json<T> {
    fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buffer.push(Value::Text(serde_json::to_string(&self.0)?));
        Ok(IsNull::No)
    }
}

impl<'r, T: serde::Deserialize<'r>> Decode<'r, PgFake> for sqlx::types::Json<T> {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = match value.value {
            Value::Json(text) => text.as_str(),
            Value::Jsonb(value) => value.get_postgres_text(),
            Value::Null => return Err(Box::new(UnexpectedNullError)),
            value => return Err(format!("cannot decode {value:?} as JSON").into()),
        };
        Ok(Self(serde_json::from_str(text)?))
    }
}

macro_rules! scalar_type {
    ($rust:ty, $base:expr, $variant:path) => {
        impl Type<PgFake> for $rust {
            fn type_info() -> PgFakeTypeInfo {
                PgFakeTypeInfo::new($base)
            }
        }

        impl<'q> Encode<'q, PgFake> for $rust {
            fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
                buffer.push($variant(self.clone()));
                Ok(IsNull::No)
            }
        }

        impl<'r> Decode<'r, PgFake> for $rust {
            fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
                match value.value {
                    $variant(value) => Ok(value.clone()),
                    Value::Null => Err(Box::new(UnexpectedNullError)),
                    value => {
                        Err(format!("cannot decode {value:?} as {}", stringify!($rust)).into())
                    }
                }
            }
        }
    };
}

scalar_type!(bool, BaseType::Bool, Value::Bool);
scalar_type!(i16, BaseType::Int2, Value::Int2);
scalar_type!(i32, BaseType::Int4, Value::Int4);
scalar_type!(i64, BaseType::Int8, Value::Int8);
scalar_type!(f32, BaseType::Float4, Value::Float4);
scalar_type!(f64, BaseType::Float8, Value::Float8);
scalar_type!(BigDecimal, BaseType::Numeric, Value::Numeric);
scalar_type!(pg_fake::jsonb::Jsonb, BaseType::Jsonb, Value::Jsonb);
scalar_type!(uuid::Uuid, BaseType::Uuid, Value::Uuid);
scalar_type!(
    pg_fake::value::PgInterval,
    BaseType::Interval,
    Value::Interval
);

impl Type<PgFake> for chrono::NaiveDate {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::Date)
    }
}

impl<'q> Encode<'q, PgFake> for chrono::NaiveDate {
    fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buffer.push(Value::Date(pg_fake::value::PgDate::Finite(*self)));
        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, PgFake> for chrono::NaiveDate {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            Value::Date(pg_fake::value::PgDate::Finite(value)) => Ok(*value),
            Value::Null => Err(Box::new(UnexpectedNullError)),
            value => Err(format!("cannot decode {value:?} as NaiveDate").into()),
        }
    }
}

impl Type<PgFake> for chrono::NaiveTime {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::Time)
    }
}

impl<'q> Encode<'q, PgFake> for chrono::NaiveTime {
    fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        use chrono::Timelike;
        buffer.push(Value::Time(pg_fake::value::PgTime(
            i64::from(self.num_seconds_from_midnight()) * 1_000_000
                + i64::from(self.nanosecond() / 1_000),
        )));
        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, PgFake> for chrono::NaiveTime {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            Value::Time(pg_fake::value::PgTime(value)) if *value < 86_400_000_000 => {
                chrono::NaiveTime::from_num_seconds_from_midnight_opt(
                    (*value / 1_000_000) as u32,
                    ((*value % 1_000_000) * 1_000) as u32,
                )
                .ok_or_else(|| "invalid time value".into())
            }
            Value::Null => Err(Box::new(UnexpectedNullError)),
            value => Err(format!("cannot decode {value:?} as NaiveTime").into()),
        }
    }
}

impl Type<PgFake> for chrono::NaiveDateTime {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::Timestamp)
    }
}

impl<'q> Encode<'q, PgFake> for chrono::NaiveDateTime {
    fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buffer.push(Value::Timestamp(pg_fake::value::PgTimestamp::Finite(*self)));
        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, PgFake> for chrono::NaiveDateTime {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            Value::Timestamp(pg_fake::value::PgTimestamp::Finite(value)) => Ok(*value),
            Value::Null => Err(Box::new(UnexpectedNullError)),
            value => Err(format!("cannot decode {value:?} as NaiveDateTime").into()),
        }
    }
}

impl Type<PgFake> for chrono::DateTime<chrono::Utc> {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::TimestampTz)
    }
}

impl<'q> Encode<'q, PgFake> for chrono::DateTime<chrono::Utc> {
    fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buffer.push(Value::TimestampTz(pg_fake::value::PgTimestampTz::Finite(
            *self,
        )));
        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, PgFake> for chrono::DateTime<chrono::Utc> {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            Value::TimestampTz(pg_fake::value::PgTimestampTz::Finite(value)) => Ok(*value),
            Value::Null => Err(Box::new(UnexpectedNullError)),
            value => Err(format!("cannot decode {value:?} as DateTime<Utc>").into()),
        }
    }
}

impl Type<PgFake> for str {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::Text)
    }

    fn compatible(type_info: &PgFakeTypeInfo) -> bool {
        matches!(
            type_info.base,
            Some(BaseType::Text | BaseType::Varchar | BaseType::Bpchar)
        )
    }
}

impl Type<PgFake> for String {
    fn type_info() -> PgFakeTypeInfo {
        <str as Type<PgFake>>::type_info()
    }

    fn compatible(type_info: &PgFakeTypeInfo) -> bool {
        <str as Type<PgFake>>::compatible(type_info)
    }
}

impl<'q> Encode<'q, PgFake> for str {
    fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buffer.push(Value::Text(self.to_owned()));
        Ok(IsNull::No)
    }
}

impl<'q> Encode<'q, PgFake> for &'q str {
    fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buffer.push(Value::Text((*self).to_owned()));
        Ok(IsNull::No)
    }
}

impl<'q> Encode<'q, PgFake> for String {
    fn encode(self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buffer.push(Value::Text(self));
        Ok(IsNull::No)
    }

    fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        self.as_str().encode_by_ref(buffer)
    }
}

impl<'r> Decode<'r, PgFake> for &'r str {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            Value::Text(value) | Value::Json(value) => Ok(value),
            Value::Null => Err(Box::new(UnexpectedNullError)),
            value => Err(format!("cannot decode {value:?} as str").into()),
        }
    }
}

impl<'r> Decode<'r, PgFake> for String {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            Value::Null => Err(Box::new(UnexpectedNullError)),
            value => Ok(value.format_postgres_text()),
        }
    }
}

impl Type<PgFake> for [u8] {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::Bytea)
    }
}

impl Type<PgFake> for Vec<u8> {
    fn type_info() -> PgFakeTypeInfo {
        <[u8] as Type<PgFake>>::type_info()
    }
}

impl<'q> Encode<'q, PgFake> for [u8] {
    fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buffer.push(Value::Bytea(self.to_vec()));
        Ok(IsNull::No)
    }
}

impl<'q> Encode<'q, PgFake> for &'q [u8] {
    fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buffer.push(Value::Bytea((*self).to_vec()));
        Ok(IsNull::No)
    }
}

impl<'q> Encode<'q, PgFake> for Vec<u8> {
    fn encode(self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buffer.push(Value::Bytea(self));
        Ok(IsNull::No)
    }

    fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        self.as_slice().encode_by_ref(buffer)
    }
}

impl<'r> Decode<'r, PgFake> for &'r [u8] {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            Value::Bytea(value) => Ok(value),
            Value::Null => Err(Box::new(UnexpectedNullError)),
            value => Err(format!("cannot decode {value:?} as bytes").into()),
        }
    }
}

impl<'r> Decode<'r, PgFake> for Vec<u8> {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        <&[u8] as Decode<PgFake>>::decode(value).map(<[u8]>::to_vec)
    }
}
