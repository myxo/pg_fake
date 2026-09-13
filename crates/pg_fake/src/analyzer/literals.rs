use crate::value::{BaseType, PgType, Value};
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn create_typed_literal(value: Value, data_type: PgType) -> ast::Expr {
    let literal = match value {
        Value::Null => ast::Value::Null,
        Value::Void => ast::Value::SingleQuotedString(String::new()),
        Value::Bool(value) => ast::Value::Boolean(value),
        Value::Int2(value) => ast::Value::Number(value.to_string(), false),
        Value::Int4(value) => ast::Value::Number(value.to_string(), false),
        Value::Int8(value) => ast::Value::Number(value.to_string(), false),
        Value::Float4(value) => {
            ast::Value::SingleQuotedString(Value::Float4(value).format_postgres_text())
        }
        Value::Float8(value) => {
            ast::Value::SingleQuotedString(Value::Float8(value).format_postgres_text())
        }
        Value::Numeric(value) => ast::Value::Number(value.to_plain_string(), false),
        Value::Text(value) => ast::Value::SingleQuotedString(value),
        Value::Bytea(value) => {
            ast::Value::SingleQuotedString(Value::Bytea(value).format_postgres_text())
        }
        Value::Uuid(value) => ast::Value::SingleQuotedString(value.to_string()),
        Value::Date(value) => {
            ast::Value::SingleQuotedString(Value::Date(value).format_postgres_text())
        }
        Value::Time(value) => {
            ast::Value::SingleQuotedString(Value::Time(value).format_postgres_text())
        }
        Value::Timestamp(value) => {
            ast::Value::SingleQuotedString(Value::Timestamp(value).format_postgres_text())
        }
        Value::TimestampTz(value) => {
            ast::Value::SingleQuotedString(Value::TimestampTz(value).format_postgres_text())
        }
        Value::Interval(value) => {
            ast::Value::SingleQuotedString(Value::Interval(value).format_postgres_text())
        }
        Value::Json(value) => ast::Value::SingleQuotedString(value),
        Value::Jsonb(value) => ast::Value::SingleQuotedString(value.get_postgres_text().to_owned()),
        Value::TextArray(values) => {
            ast::Value::SingleQuotedString(crate::text_array::format_array(&values))
        }
    };
    create_typed_cast(ast::Expr::Value(literal.into()), data_type)
}

pub(crate) fn create_typed_cast(expression: ast::Expr, data_type: PgType) -> ast::Expr {
    ast::Expr::Cast {
        kind: ast::CastKind::Cast,
        expr: Box::new(expression),
        data_type: convert_to_ast_data_type(data_type),
        format: None,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn convert_to_ast_data_type(data_type: PgType) -> ast::DataType {
    match data_type.base {
        BaseType::Void => ast::DataType::Custom(ast::Ident::new("void").into(), Vec::new()),
        BaseType::Bool => ast::DataType::Boolean,
        BaseType::Int2 => ast::DataType::SmallInt(None),
        BaseType::Int4 => ast::DataType::Integer(None),
        BaseType::Int8 => ast::DataType::BigInt(None),
        BaseType::Float4 => ast::DataType::Real,
        BaseType::Float8 => ast::DataType::DoublePrecision,
        BaseType::Numeric if data_type.typmod == PgType::NO_TYPEMOD => {
            ast::DataType::Numeric(ast::ExactNumberInfo::None)
        }
        BaseType::Numeric => {
            let encoded = data_type.typmod - 4;
            ast::DataType::Numeric(ast::ExactNumberInfo::PrecisionAndScale(
                u64::try_from(encoded >> 16).expect("valid numeric precision"),
                i64::from(encoded & 0xffff),
            ))
        }
        BaseType::Text => ast::DataType::Text,
        BaseType::Varchar if data_type.typmod == PgType::NO_TYPEMOD => ast::DataType::Varchar(None),
        BaseType::Varchar => ast::DataType::Varchar(Some(ast::CharacterLength::IntegerLength {
            length: u64::try_from(data_type.typmod - 4).expect("valid character typmod"),
            unit: None,
        })),
        BaseType::Bpchar if data_type.typmod == PgType::NO_TYPEMOD => {
            ast::DataType::Custom(ast::Ident::new("bpchar").into(), Vec::new())
        }
        BaseType::Bpchar => ast::DataType::Char(Some(ast::CharacterLength::IntegerLength {
            length: u64::try_from(data_type.typmod - 4).expect("valid character typmod"),
            unit: None,
        })),
        BaseType::Bytea => ast::DataType::Bytea,
        BaseType::Uuid => ast::DataType::Uuid,
        BaseType::Date => ast::DataType::Date,
        BaseType::Time => ast::DataType::Time(
            (data_type.typmod != PgType::NO_TYPEMOD)
                .then(|| u64::try_from(data_type.typmod).expect("valid time precision")),
            ast::TimezoneInfo::WithoutTimeZone,
        ),
        BaseType::Timestamp => ast::DataType::Timestamp(
            (data_type.typmod != PgType::NO_TYPEMOD)
                .then(|| u64::try_from(data_type.typmod).expect("valid timestamp precision")),
            ast::TimezoneInfo::WithoutTimeZone,
        ),
        BaseType::TimestampTz => ast::DataType::Timestamp(
            (data_type.typmod != PgType::NO_TYPEMOD)
                .then(|| u64::try_from(data_type.typmod).expect("valid timestamptz precision")),
            ast::TimezoneInfo::WithTimeZone,
        ),
        BaseType::Interval => ast::DataType::Interval {
            fields: None,
            precision: None,
        },
        BaseType::Json => ast::DataType::JSON,
        BaseType::Jsonb => ast::DataType::JSONB,
        BaseType::TextArray => ast::DataType::Array(ast::ArrayElemTypeDef::SquareBracket(
            Box::new(ast::DataType::Text),
            None,
        )),
    }
}
