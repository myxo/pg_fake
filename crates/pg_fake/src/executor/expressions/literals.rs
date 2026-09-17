use crate::executor::arithmetic::evaluate_unary_operator;
use crate::{
    coercion,
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, Value},
};
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn extract_ast_value(expr: &ast::Expr) -> Option<&ast::Value> {
    let ast::Expr::Value(value) = expr else {
        return None;
    };
    Some(&value.value)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn extract_number_literal(expr: &ast::Expr) -> Option<&str> {
    let ast::Value::Number(value, _) = extract_ast_value(expr)? else {
        return None;
    };
    Some(value)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn is_parameter_placeholder(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Nested(inner) => is_parameter_placeholder(inner),
        _ => matches!(extract_ast_value(expr), Some(ast::Value::Placeholder(_))),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn evaluate_literal(expr: &ast::Expr) -> Result<Value> {
    if let Some(value) = extract_ast_value(expr) {
        return match value {
            ast::Value::Null => Ok(Value::Null),
            ast::Value::Boolean(value) => Ok(Value::Bool(*value)),
            ast::Value::SingleQuotedString(value) => Ok(Value::Text(value.clone())),
            ast::Value::DollarQuotedString(value) => Ok(Value::Text(value.value.clone())),
            ast::Value::Number(value, _) if value.contains(['.', 'e', 'E']) => {
                Value::parse(BaseType::Numeric, value)
            }
            ast::Value::Number(value, _) => parse_integer_literal(value),
            _ => Err(PgError::create(
                SqlState::CannotCoerce,
                "literal has incompatible type",
            )),
        };
    }
    match expr {
        ast::Expr::TypedString(typed) if !typed.uses_odbc_syntax => {
            let target = coercion::convert_ast_data_type(&typed.data_type)?;
            let ast::Value::SingleQuotedString(text) = &typed.value.value else {
                return reject_unsupported("typed literal is not implemented");
            };
            if !matches!(
                target.base,
                BaseType::Json
                    | BaseType::Jsonb
                    | BaseType::Date
                    | BaseType::Timestamp
                    | BaseType::TimestampTz
                    | BaseType::PgLsn
                    | BaseType::Oid
                    | BaseType::Regclass
            ) {
                return reject_unsupported("typed literal is not implemented");
            }
            Value::parse(target.base, text)
        }
        ast::Expr::UnaryOp {
            op: ast::UnaryOperator::Plus,
            expr,
        } => evaluate_literal(expr),
        ast::Expr::UnaryOp {
            op: ast::UnaryOperator::Minus,
            expr,
        } if extract_number_literal(expr).is_some_and(|value| !value.contains(['.', 'e', 'E'])) => {
            let value = extract_number_literal(expr).expect("integer literal pattern was checked");
            parse_integer_literal(&format!("-{value}"))
        }
        ast::Expr::UnaryOp {
            op: ast::UnaryOperator::Minus,
            expr,
        } if extract_number_literal(expr).is_some() => {
            evaluate_unary_operator(ast::UnaryOperator::Minus, evaluate_literal(expr)?)
        }
        ast::Expr::Nested(expr) => evaluate_literal(expr),
        _ => reject_unsupported("expression is not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn parse_integer_literal(value: &str) -> Result<Value> {
    if let Ok(value) = value.parse::<i32>() {
        return Ok(Value::Int4(value));
    }
    if let Ok(value) = value.parse::<i64>() {
        return Ok(Value::Int8(value));
    }
    Value::parse(BaseType::Numeric, value)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn extract_unknown_string_literal(expr: &ast::Expr) -> Option<&str> {
    match expr {
        ast::Expr::Value(value) => match &value.value {
            ast::Value::SingleQuotedString(value) => Some(value),
            ast::Value::DollarQuotedString(value) => Some(&value.value),
            _ => None,
        },
        ast::Expr::Nested(expr) => extract_unknown_string_literal(expr),
        _ => None,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn is_null_literal(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Value(value) if matches!(&value.value, ast::Value::Null) => true,
        ast::Expr::Nested(expr) => is_null_literal(expr),
        _ => false,
    }
}
