use super::{
    evaluate_and_coerce, extract_unknown_string_literal, infer_expression_type, is_null_literal,
    resolve_operator_type,
};
use crate::{
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{StatementContext, scope::RowScope},
    value::{BaseType, Value},
};
use sqlparser::ast;

pub(crate) struct UnnestTableFunction<'a> {
    pub(crate) argument: &'a ast::Expr,
    pub(crate) alias: Option<&'a ast::TableAlias>,
    pub(crate) ordinality: bool,
}

pub(crate) fn extract_unnest_table_function(
    factor: &ast::TableFactor,
) -> Result<Option<UnnestTableFunction<'_>>> {
    let (name, args, alias, ordinality) = match factor {
        ast::TableFactor::UNNEST {
            alias,
            array_exprs,
            with_offset,
            with_ordinality,
            ..
        } => {
            if *with_offset {
                return reject_unsupported("UNNEST WITH OFFSET is not implemented");
            }
            let [argument] = array_exprs.as_slice() else {
                return reject_unsupported("multiple-array unnest is not implemented");
            };
            return Ok(Some(UnnestTableFunction {
                argument,
                alias: alias.as_ref(),
                ordinality: *with_ordinality,
            }));
        }
        ast::TableFactor::Table {
            name,
            args: Some(args),
            alias,
            with_ordinality,
            ..
        } => {
            if args.settings.is_some() {
                return reject_unsupported("table function settings are not implemented");
            }
            (name, &args.args, alias.as_ref(), *with_ordinality)
        }
        ast::TableFactor::Function {
            name,
            args,
            alias,
            with_ordinality,
            ..
        } => (name, args, alias.as_ref(), *with_ordinality),
        _ => return Ok(None),
    };
    if crate::executor::normalize_function_name(name)? != "unnest" {
        return Ok(None);
    }
    let [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))] = args.as_slice() else {
        return Err(PgError::create(
            SqlState::UndefinedFunction,
            "function unnest does not exist",
        ));
    };
    Ok(Some(UnnestTableFunction {
        argument,
        alias,
        ordinality,
    }))
}

pub(super) fn evaluate_array_operator(
    expression: &ast::Expr,
    left: &ast::Expr,
    operator: &ast::BinaryOperator,
    right: &ast::Expr,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    if *operator == ast::BinaryOperator::StringConcat {
        let result_type = infer_expression_type(expression, schema)?;
        let element_type = result_type
            .get_array_element_type()
            .expect("array concatenation returns an array");
        let left_type = infer_expression_type(left, schema)?;
        let right_type = infer_expression_type(right, schema)?;
        let left_declared_array = left_type.get_array_element_type().is_some();
        let right_declared_array = right_type.get_array_element_type().is_some();
        let left_unknown = is_null_literal(left) || extract_unknown_string_literal(left).is_some();
        let right_unknown =
            is_null_literal(right) || extract_unknown_string_literal(right).is_some();
        let left_is_array = left_declared_array || left_unknown && right_declared_array;
        let right_is_array = right_declared_array || right_unknown && left_declared_array;
        let left = if left_is_array {
            evaluate_and_coerce(
                left,
                result_type,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?
        } else {
            evaluate_and_coerce(
                left,
                element_type,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?
        };
        let right = if right_is_array {
            evaluate_and_coerce(
                right,
                result_type,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?
        } else {
            evaluate_and_coerce(
                right,
                element_type,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?
        };
        if left_is_array && right_is_array && left.is_null() && right.is_null() {
            return Ok(Value::Null);
        }
        let mut values = match (left_is_array, left) {
            (true, Value::Array { values, .. }) => values,
            (true, Value::Null) => Vec::new(),
            (false, value) => vec![value],
            _ => unreachable!("array concatenation operand was type-checked"),
        };
        match (right_is_array, right) {
            (true, Value::Array { values: right, .. }) => values.extend(right),
            (true, Value::Null) => {}
            (false, value) => values.push(value),
            _ => unreachable!("array concatenation operand was type-checked"),
        }
        return Ok(Value::Array {
            elem_type: element_type,
            values,
        });
    }

    let target = resolve_operator_type(left, right, schema)?;
    let target_element = target
        .get_array_element_type()
        .expect("array operator target is an array");
    let left = evaluate_and_coerce(left, target, CastContext::Implicit, schema, row, context)?;
    let right = evaluate_and_coerce(right, target, CastContext::Implicit, schema, row, context)?;
    let (
        Value::Array {
            values: left_values,
            ..
        },
        Value::Array {
            values: right_values,
            ..
        },
    ) = (left, right)
    else {
        return Ok(Value::Null);
    };
    let contains = |haystack: &[Value], needles: &[Value]| -> Result<bool> {
        for needle in needles {
            let mut found = false;
            for candidate in haystack {
                found = match (candidate, needle) {
                    (Value::Null, _) | (_, Value::Null) => false,
                    _ => super::compare_values_as_type(candidate, needle, target_element)?.is_eq(),
                };
                if found {
                    break;
                }
            }
            if !found {
                return Ok(false);
            }
        }
        Ok(true)
    };
    Ok(Value::Bool(match operator {
        ast::BinaryOperator::AtArrow => contains(&left_values, &right_values)?,
        ast::BinaryOperator::ArrowAt => contains(&right_values, &left_values)?,
        ast::BinaryOperator::PGOverlap => {
            let mut overlaps = false;
            for value in &right_values {
                if contains(&left_values, std::slice::from_ref(value))? {
                    overlaps = true;
                    break;
                }
            }
            overlaps
        }
        _ => return reject_unsupported("array operator is not implemented"),
    }))
}

pub(super) fn infer_array_function(
    name: &str,
    arguments: &[&ast::Expr],
    schema: RowScope<'_>,
) -> Result<Option<BaseType>> {
    let signature_error = || {
        PgError::create(
            SqlState::UndefinedFunction,
            format!("function {name} does not exist"),
        )
    };
    let get_array = |expression: &ast::Expr| -> Result<(BaseType, BaseType)> {
        let array_type = infer_expression_type(expression, schema)?;
        let element_type = array_type
            .get_array_element_type()
            .ok_or_else(&signature_error)?;
        Ok((array_type, element_type))
    };
    let get_known_array = |expression: &ast::Expr| -> Result<Option<(BaseType, BaseType)>> {
        if is_null_literal(expression) || extract_unknown_string_literal(expression).is_some() {
            return Ok(None);
        }
        get_array(expression).map(Some)
    };
    let validate_int4 = |expression: &ast::Expr| -> Result<()> {
        super::functions::validate_function_argument(
            expression,
            BaseType::Int4,
            schema,
            &signature_error,
        )
    };
    let resolve_element = |element_type: BaseType,
                           expressions: &[&ast::Expr]|
     -> Result<BaseType> {
        let mut common = element_type;
        for expression in expressions {
            if is_null_literal(expression) || extract_unknown_string_literal(expression).is_some() {
                continue;
            }
            let source = infer_expression_type(expression, schema)?;
            common = coercion::resolve_common_type(common, source)
                .filter(|target| {
                    coercion::can_cast(element_type, *target, CastContext::Implicit)
                        && coercion::can_cast(source, *target, CastContext::Implicit)
                })
                .ok_or_else(&signature_error)?;
        }
        Ok(common)
    };
    let infer_element = |array: &ast::Expr, expressions: &[&ast::Expr]| -> Result<BaseType> {
        if let Some((_, element_type)) = get_known_array(array)? {
            return resolve_element(element_type, expressions);
        }
        let mut common = None;
        for expression in expressions {
            if is_null_literal(expression) || extract_unknown_string_literal(expression).is_some() {
                continue;
            }
            let source = infer_expression_type(expression, schema)?;
            common = Some(match common {
                None => source,
                Some(previous) => coercion::resolve_common_type(previous, source)
                    .filter(|target| {
                        coercion::can_cast(previous, *target, CastContext::Implicit)
                            && coercion::can_cast(source, *target, CastContext::Implicit)
                    })
                    .ok_or_else(&signature_error)?,
            });
        }
        common.ok_or_else(signature_error)
    };
    Ok(Some(match (name, arguments) {
        ("cardinality", [array]) => {
            get_array(array)?;
            BaseType::Int4
        }
        ("array_length" | "array_lower" | "array_upper", [array, dimension]) => {
            get_array(array)?;
            validate_int4(dimension)?;
            BaseType::Int4
        }
        ("array_append", [array, value]) => infer_element(array, &[value])?
            .get_array_type()
            .expect("scalar has an array type"),
        ("array_prepend", [value, array]) => infer_element(array, &[value])?
            .get_array_type()
            .expect("scalar has an array type"),
        ("array_cat", [left, right]) => match (get_known_array(left)?, get_known_array(right)?) {
            (Some((left, _)), Some((right, _))) => {
                coercion::resolve_common_type(left, right).ok_or_else(signature_error)?
            }
            (Some((array, _)), None) | (None, Some((array, _))) => array,
            (None, None) => return Err(signature_error()),
        },
        ("array_position", [array, value] | [array, value, _]) => {
            let element_type = infer_element(array, &[value])?;
            super::validate_equality_type(element_type)?;
            if let Some(start) = arguments.get(2) {
                validate_int4(start)?;
            }
            BaseType::Int4
        }
        ("array_positions", [array, value]) => {
            let element_type = infer_element(array, &[value])?;
            super::validate_equality_type(element_type)?;
            BaseType::Int4.get_array_type().unwrap()
        }
        ("array_remove", [array, value]) => {
            let element_type = infer_element(array, &[value])?;
            super::validate_equality_type(element_type)?;
            element_type
                .get_array_type()
                .expect("scalar has an array type")
        }
        ("array_replace", [array, search, replacement]) => {
            let element_type = infer_element(array, &[search, replacement])?;
            super::validate_equality_type(element_type)?;
            element_type
                .get_array_type()
                .expect("scalar has an array type")
        }
        (
            "cardinality" | "array_length" | "array_lower" | "array_upper" | "array_append"
            | "array_prepend" | "array_cat" | "array_position" | "array_positions" | "array_remove"
            | "array_replace",
            _,
        ) => return Err(signature_error()),
        _ => return Ok(None),
    }))
}

pub(super) fn evaluate_array_function(
    name: &str,
    arguments: &[&ast::Expr],
    result_type: BaseType,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    let array_index = if name == "array_prepend" { 1 } else { 0 };
    let source_array_type = infer_expression_type(arguments[array_index], schema)?;
    let source_element_type = source_array_type.get_array_element_type();
    let mut element_type = if matches!(
        name,
        "array_append" | "array_prepend" | "array_cat" | "array_remove" | "array_replace"
    ) {
        result_type
            .get_array_element_type()
            .expect("array mutation function returns an array")
    } else {
        source_element_type.unwrap_or_else(|| {
            infer_expression_type(arguments[1], schema)
                .expect("array search function was type-checked")
        })
    };
    if matches!(name, "array_position" | "array_positions")
        && let Some(source_element_type) = source_element_type
        && !is_null_literal(arguments[1])
        && extract_unknown_string_literal(arguments[1]).is_none()
    {
        element_type = coercion::resolve_common_type(
            source_element_type,
            infer_expression_type(arguments[1], schema)?,
        )
        .expect("array search types were checked");
    }
    let array_type = element_type
        .get_array_type()
        .expect("array function element has an array type");
    let evaluate_array = |expression: &ast::Expr, target: BaseType| {
        evaluate_and_coerce(
            expression,
            target,
            CastContext::Implicit,
            schema,
            row,
            context,
        )
    };
    let evaluate_element = |expression: &ast::Expr| {
        evaluate_and_coerce(
            expression,
            element_type,
            CastContext::Implicit,
            schema,
            row,
            context,
        )
    };
    let values = match evaluate_array(arguments[array_index], array_type)? {
        Value::Array { values, .. } => Some(values),
        Value::Null => None,
        _ => unreachable!("array function argument was type-checked"),
    };
    match name {
        "cardinality" => Ok(values
            .map(|values| Value::Int4(i32::try_from(values.len()).expect("array length fits int4")))
            .unwrap_or(Value::Null)),
        "array_length" | "array_lower" | "array_upper" => {
            let dimension = evaluate_and_coerce(
                arguments[1],
                BaseType::Int4,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let (Some(values), Value::Int4(1)) = (values, dimension) else {
                return Ok(Value::Null);
            };
            if values.is_empty() {
                return Ok(Value::Null);
            }
            Ok(Value::Int4(if name == "array_lower" {
                1
            } else {
                i32::try_from(values.len()).expect("array length fits int4")
            }))
        }
        "array_append" | "array_prepend" => {
            let mut values = values.unwrap_or_default();
            let value = evaluate_element(arguments[1 - array_index])?;
            if name == "array_append" {
                values.push(value);
            } else {
                values.insert(0, value);
            }
            Ok(Value::Array {
                elem_type: element_type,
                values,
            })
        }
        "array_cat" => {
            let result_element = result_type
                .get_array_element_type()
                .expect("array_cat returns an array");
            let left = evaluate_array(arguments[0], result_type)?;
            let right = evaluate_array(arguments[1], result_type)?;
            if left.is_null() && right.is_null() {
                return Ok(Value::Null);
            }
            let mut left = match left {
                Value::Array { values, .. } => values,
                Value::Null => Vec::new(),
                _ => unreachable!("array_cat argument was type-checked"),
            };
            if let Value::Array { values, .. } = right {
                left.extend(values);
            }
            Ok(Value::Array {
                elem_type: result_element,
                values: left,
            })
        }
        "array_position" | "array_positions" | "array_remove" | "array_replace" => {
            let Some(values) = values else {
                return Ok(Value::Null);
            };
            let search = evaluate_element(arguments[1])?;
            let matches = |value: &Value| -> Result<bool> {
                Ok(match (value, &search) {
                    (Value::Null, Value::Null) => true,
                    (Value::Null, _) | (_, Value::Null) => false,
                    _ => super::compare_values_as_type(value, &search, element_type)?.is_eq(),
                })
            };
            if name == "array_position" {
                let start = if let Some(start) = arguments.get(2) {
                    match evaluate_and_coerce(
                        start,
                        BaseType::Int4,
                        CastContext::Implicit,
                        schema,
                        row,
                        context,
                    )? {
                        Value::Int4(start) if start > 0 => usize::try_from(start - 1).unwrap(),
                        Value::Int4(_) => 0,
                        Value::Null => return Ok(Value::Null),
                        _ => unreachable!("array_position start was type-checked"),
                    }
                } else {
                    0
                };
                for (index, value) in values.iter().enumerate().skip(start) {
                    if matches(value)? {
                        return Ok(Value::Int4(i32::try_from(index + 1).unwrap()));
                    }
                }
                return Ok(Value::Null);
            }
            if name == "array_positions" {
                let mut positions = Vec::new();
                for (index, value) in values.iter().enumerate() {
                    if matches(value)? {
                        positions.push(Value::Int4(i32::try_from(index + 1).unwrap()));
                    }
                }
                return Ok(Value::Array {
                    elem_type: BaseType::Int4,
                    values: positions,
                });
            }
            let replacement = if name == "array_replace" {
                Some(evaluate_element(arguments[2])?)
            } else {
                None
            };
            let mut result = Vec::with_capacity(values.len());
            for value in values {
                if matches(&value)? {
                    if let Some(replacement) = &replacement {
                        result.push(replacement.clone());
                    }
                } else {
                    result.push(value);
                }
            }
            Ok(Value::Array {
                elem_type: element_type,
                values: result,
            })
        }
        _ => unreachable!("array function was resolved"),
    }
}
