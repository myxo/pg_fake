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
            Some(BaseType::Void) => "VOID",
            Some(BaseType::Bool) => "BOOL",
            Some(BaseType::Int2) => "INT2",
            Some(BaseType::Int4) => "INT4",
            Some(BaseType::Oid) => "OID",
            Some(BaseType::Int8) => "INT8",
            Some(BaseType::Float4) => "FLOAT4",
            Some(BaseType::Float8) => "FLOAT8",
            Some(BaseType::Numeric) => "NUMERIC",
            Some(BaseType::Text) => "TEXT",
            Some(BaseType::Varchar) => "VARCHAR",
            Some(BaseType::Bpchar) => "CHAR",
            Some(BaseType::Bytea) => "BYTEA",
            Some(BaseType::Uuid) => "UUID",
            Some(BaseType::Date) => "DATE",
            Some(BaseType::Time) => "TIME",
            Some(BaseType::Timestamp) => "TIMESTAMP",
            Some(BaseType::TimestampTz) => "TIMESTAMPTZ",
            Some(BaseType::Interval) => "INTERVAL",
            Some(BaseType::Json) => "JSON",
            Some(BaseType::Jsonb) => "JSONB",
            Some(BaseType::PgLsn) => "PG_LSN",
            Some(BaseType::Regclass) => "REGCLASS",
            Some(BaseType::TextArray) => "TEXT[]",
            Some(BaseType::Int8Array) => "INT8[]",
            Some(BaseType::UuidArray) => "UUID[]",
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

impl<'q> Arguments<'q> for PgFakeArguments {
    type Database = PgFake;

    fn reserve(&mut self, additional: usize, _size: usize) {
        self.values.reserve(additional);
        self.types.reserve(additional);
    }

    fn add<T>(&mut self, value: T) -> Result<(), BoxDynError>
    where
        T: 'q + Encode<'q, Self::Database> + Type<Self::Database>,
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
scalar_type!(u32, BaseType::Oid, Value::Oid);
scalar_type!(i64, BaseType::Int8, Value::Int8);
scalar_type!(f32, BaseType::Float4, Value::Float4);
scalar_type!(f64, BaseType::Float8, Value::Float8);
scalar_type!(BigDecimal, BaseType::Numeric, Value::Numeric);
scalar_type!(pg_fake::jsonb::Jsonb, BaseType::Jsonb, Value::Jsonb);
scalar_type!(uuid::Uuid, BaseType::Uuid, Value::Uuid);
scalar_type!(pg_fake::value::PgLsn, BaseType::PgLsn, Value::PgLsn);
scalar_type!(
    pg_fake::value::PgRegclass,
    BaseType::Regclass,
    Value::Regclass
);
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

#[cfg(feature = "time")]
impl Type<PgFake> for time::OffsetDateTime {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::TimestampTz)
    }
}

#[cfg(feature = "time")]
impl<'q> Encode<'q, PgFake> for time::OffsetDateTime {
    fn encode_by_ref(&self, buffer: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        const POSTGRES_EPOCH_NANOS: i128 = 946_684_800_000_000_000;

        let postgres_nanos = self.unix_timestamp_nanos() - POSTGRES_EPOCH_NANOS;
        let postgres_micros = postgres_nanos / 1_000;
        let unix_nanos = POSTGRES_EPOCH_NANOS + postgres_micros * 1_000;
        let seconds = unix_nanos.div_euclid(1_000_000_000);
        let nanoseconds = unix_nanos.rem_euclid(1_000_000_000);
        let seconds = i64::try_from(seconds)?;
        let nanoseconds = u32::try_from(nanoseconds)?;
        let value = chrono::DateTime::from_timestamp(seconds, nanoseconds)
            .ok_or("OffsetDateTime is outside pg_fake's timestamptz range")?;
        buffer.push(Value::TimestampTz(pg_fake::value::PgTimestampTz::Finite(
            value,
        )));
        Ok(IsNull::No)
    }
}

#[cfg(feature = "time")]
impl<'r> Decode<'r, PgFake> for time::OffsetDateTime {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            Value::TimestampTz(pg_fake::value::PgTimestampTz::Finite(value)) => {
                const POSTGRES_EPOCH_NANOS: i128 = 946_684_800_000_000_000;

                let unix_nanos = i128::from(value.timestamp()) * 1_000_000_000
                    + i128::from(value.timestamp_subsec_nanos());
                let postgres_nanos = unix_nanos - POSTGRES_EPOCH_NANOS;
                let unix_nanos = POSTGRES_EPOCH_NANOS + postgres_nanos / 1_000 * 1_000;
                Ok(time::OffsetDateTime::from_unix_timestamp_nanos(unix_nanos)?)
            }
            Value::Null => Err(Box::new(UnexpectedNullError)),
            value => Err(format!("cannot decode {value:?} as OffsetDateTime").into()),
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

impl Type<PgFake> for () {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::Void)
    }
}

impl<'r> Decode<'r, PgFake> for () {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            Value::Void => Ok(()),
            Value::Null => Err(Box::new(UnexpectedNullError)),
            value => Err(format!("cannot decode {value:?} as void").into()),
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

impl Type<PgFake> for Vec<Option<String>> {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::TextArray)
    }
}
impl<'q> Encode<'q, PgFake> for Vec<Option<String>> {
    fn encode_by_ref(&self, buf: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buf.push(Value::Array {
            elem_type: BaseType::Text,
            values: self
                .iter()
                .map(|value| value.clone().map(Value::Text).unwrap_or(Value::Null))
                .collect(),
        });
        Ok(IsNull::No)
    }
}
impl<'r> Decode<'r, PgFake> for Vec<Option<String>> {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            Value::Array {
                elem_type: BaseType::Text,
                values,
            } => values
                .iter()
                .map(|value| match value {
                    Value::Null => Ok(None),
                    Value::Text(value) => Ok(Some(value.clone())),
                    _ => Err("expected text array".into()),
                })
                .collect(),
            Value::Null => Err(Box::new(UnexpectedNullError)),
            _ => Err("expected text array".into()),
        }
    }
}
impl Type<PgFake> for Vec<String> {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::TextArray)
    }
}
impl<'q> Encode<'q, PgFake> for Vec<String> {
    fn encode_by_ref(&self, buf: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buf.push(Value::Array {
            elem_type: BaseType::Text,
            values: self.iter().cloned().map(Value::Text).collect(),
        });
        Ok(IsNull::No)
    }
}
impl<'r> Decode<'r, PgFake> for Vec<String> {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        <Vec<Option<String>> as Decode<PgFake>>::decode(value)?
            .into_iter()
            .map(|v| v.ok_or_else(|| "unexpected null array element".into()))
            .collect()
    }
}

impl Type<PgFake> for Vec<Option<i64>> {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::Int8Array)
    }
}
impl<'q> Encode<'q, PgFake> for Vec<Option<i64>> {
    fn encode_by_ref(&self, buf: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buf.push(Value::Array {
            elem_type: BaseType::Int8,
            values: self
                .iter()
                .map(|value| value.map(Value::Int8).unwrap_or(Value::Null))
                .collect(),
        });
        Ok(IsNull::No)
    }
}
impl<'r> Decode<'r, PgFake> for Vec<Option<i64>> {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            Value::Array {
                elem_type: BaseType::Int8,
                values,
            } => values
                .iter()
                .map(|value| match value {
                    Value::Null => Ok(None),
                    Value::Int8(value) => Ok(Some(*value)),
                    _ => Err("expected bigint array".into()),
                })
                .collect(),
            Value::Null => Err(Box::new(UnexpectedNullError)),
            _ => Err("expected bigint array".into()),
        }
    }
}
impl Type<PgFake> for Vec<i64> {
    fn type_info() -> PgFakeTypeInfo {
        <Vec<Option<i64>> as Type<PgFake>>::type_info()
    }
}
impl<'q> Encode<'q, PgFake> for Vec<i64> {
    fn encode_by_ref(&self, buf: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buf.push(Value::Array {
            elem_type: BaseType::Int8,
            values: self.iter().copied().map(Value::Int8).collect(),
        });
        Ok(IsNull::No)
    }
}
impl<'r> Decode<'r, PgFake> for Vec<i64> {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        <Vec<Option<i64>> as Decode<PgFake>>::decode(value)?
            .into_iter()
            .map(|value| value.ok_or_else(|| "unexpected null array element".into()))
            .collect()
    }
}

impl Type<PgFake> for Vec<Option<uuid::Uuid>> {
    fn type_info() -> PgFakeTypeInfo {
        PgFakeTypeInfo::new(BaseType::UuidArray)
    }
}
impl<'q> Encode<'q, PgFake> for Vec<Option<uuid::Uuid>> {
    fn encode_by_ref(&self, buf: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buf.push(Value::Array {
            elem_type: BaseType::Uuid,
            values: self
                .iter()
                .map(|value| value.map(Value::Uuid).unwrap_or(Value::Null))
                .collect(),
        });
        Ok(IsNull::No)
    }
}
impl<'r> Decode<'r, PgFake> for Vec<Option<uuid::Uuid>> {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            Value::Array {
                elem_type: BaseType::Uuid,
                values,
            } => values
                .iter()
                .map(|value| match value {
                    Value::Null => Ok(None),
                    Value::Uuid(value) => Ok(Some(*value)),
                    _ => Err("expected UUID array".into()),
                })
                .collect(),
            Value::Null => Err(Box::new(UnexpectedNullError)),
            _ => Err("expected UUID array".into()),
        }
    }
}
impl Type<PgFake> for Vec<uuid::Uuid> {
    fn type_info() -> PgFakeTypeInfo {
        <Vec<Option<uuid::Uuid>> as Type<PgFake>>::type_info()
    }
}
impl<'q> Encode<'q, PgFake> for Vec<uuid::Uuid> {
    fn encode_by_ref(&self, buf: &mut Vec<Value>) -> Result<IsNull, BoxDynError> {
        buf.push(Value::Array {
            elem_type: BaseType::Uuid,
            values: self.iter().copied().map(Value::Uuid).collect(),
        });
        Ok(IsNull::No)
    }
}
impl<'r> Decode<'r, PgFake> for Vec<uuid::Uuid> {
    fn decode(value: PgFakeValueRef<'r>) -> Result<Self, BoxDynError> {
        <Vec<Option<uuid::Uuid>> as Decode<PgFake>>::decode(value)?
            .into_iter()
            .map(|value| value.ok_or_else(|| "unexpected null array element".into()))
            .collect()
    }
}
