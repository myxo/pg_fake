use super::super::parse_placeholder_index;
use crate::{
    coercion,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor,
    value::BaseType,
};
use sqlparser::ast;
use std::ops::ControlFlow;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn infer_expression_parameters(
    expression: &ast::Expr,
    schema: executor::RowScope<'_>,
    expected: Option<BaseType>,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    constrain_parameter_type(expression, expected, types)?;
    let mut error = None;
    let _ = ast::visit_expressions(expression, |expression| {
        let result = match expression {
            ast::Expr::Identifier(identifier)
                if identifier.quote_style.is_none()
                    && identifier.value.eq_ignore_ascii_case("default") =>
            {
                Ok(())
            }
            ast::Expr::Identifier(_) => {
                executor::infer_expression_type(expression, schema).map(|_| ())
            }
            ast::Expr::Nested(_) => Ok(()),
            ast::Expr::CompoundFieldAccess { root, access_chain } => {
                let [ast::AccessExpr::Subscript(ast::Subscript::Index { index })] =
                    access_chain.as_slice()
                else {
                    return ControlFlow::Continue(());
                };
                constrain_parameter_type(root, expected, types)
                    .and_then(|()| constrain_parameter_type(index, Some(BaseType::Int4), types))
            }
            ast::Expr::Cast {
                expr, data_type, ..
            } => coercion::convert_ast_data_type(data_type).and_then(|target| {
                if let ast::Expr::Array(array) = expr.as_ref()
                    && let Some(elem_type) = target.base.get_array_element_type()
                {
                    array.elem.iter().try_for_each(|element| {
                        constrain_parameter_type(element, Some(elem_type), types)
                    })
                } else if infer_parameter_expression_type(expr, schema, types).is_none() {
                    constrain_parameter_type(expr, Some(target.base), types)
                } else {
                    Ok(())
                }
            }),
            ast::Expr::Array(array) => {
                if let Some(elem_type) = expected.and_then(BaseType::get_array_element_type) {
                    array.elem.iter().try_for_each(|element| {
                        constrain_parameter_type(element, Some(elem_type), types)
                    })
                } else {
                    Ok(())
                }
            }
            ast::Expr::UnaryOp { op, expr } => constrain_parameter_type(
                expr,
                matches!(op, ast::UnaryOperator::Not).then_some(BaseType::Bool),
                types,
            ),
            ast::Expr::Like { expr, pattern, .. }
            | ast::Expr::ILike { expr, pattern, .. }
            | ast::Expr::BinaryOp {
                left: expr,
                right: pattern,
                op:
                    ast::BinaryOperator::PGRegexMatch
                    | ast::BinaryOperator::PGRegexIMatch
                    | ast::BinaryOperator::PGRegexNotMatch
                    | ast::BinaryOperator::PGRegexNotIMatch,
            } => (|| {
                for argument in [expr, pattern] {
                    if infer_parameter_expression_type(argument, schema, types).is_none() {
                        constrain_parameter_type(argument, Some(BaseType::Text), types)?;
                    }
                }
                Ok(())
            })(),
            ast::Expr::BinaryOp { left, op, right } => {
                if executor::resolve_json_operator_types(
                    op,
                    Some(BaseType::Jsonb),
                    Some(BaseType::Jsonb),
                )
                .is_some()
                {
                    let left_type = infer_parameter_expression_type(left, schema, types);
                    let right_type = infer_parameter_expression_type(right, schema, types);
                    if left_type.is_none()
                        && matches!(
                            op,
                            ast::BinaryOperator::Arrow
                                | ast::BinaryOperator::LongArrow
                                | ast::BinaryOperator::HashArrow
                                | ast::BinaryOperator::HashLongArrow
                        )
                    {
                        error = Some(PgError::create(
                            SqlState::AmbiguousFunction,
                            "operator is not unique",
                        ));
                        return ControlFlow::Break(());
                    }
                    if let Some((l, r, _)) =
                        executor::resolve_json_operator_types(op, left_type, right_type)
                    {
                        return match constrain_parameter_type(left, Some(l), types)
                            .and_then(|()| constrain_parameter_type(right, Some(r), types))
                        {
                            Ok(()) => ControlFlow::Continue(()),
                            Err(e) => {
                                error = Some(e);
                                ControlFlow::Break(())
                            }
                        };
                    }
                }
                let boolean = matches!(op, ast::BinaryOperator::And | ast::BinaryOperator::Or);
                let left_expected = if boolean {
                    Some(BaseType::Bool)
                } else {
                    executor::infer_expression_type(right, schema)
                        .ok()
                        .map(|base| {
                            if base == BaseType::Varchar {
                                BaseType::Text
                            } else {
                                base
                            }
                        })
                };
                let right_expected = if boolean {
                    Some(BaseType::Bool)
                } else {
                    executor::infer_expression_type(left, schema)
                        .ok()
                        .map(|base| {
                            if base == BaseType::Varchar {
                                BaseType::Text
                            } else {
                                base
                            }
                        })
                };
                constrain_parameter_type(left, left_expected, types)
                    .and_then(|()| constrain_parameter_type(right, right_expected, types))
            }
            ast::Expr::Floor { expr, .. } => {
                let target = infer_parameter_expression_type(expr, schema, types)
                    .unwrap_or(BaseType::Float8);
                constrain_parameter_type(expr, Some(target), types)
            }
            ast::Expr::AtTimeZone {
                timestamp,
                time_zone,
            } => (|| {
                for (argument, default) in [
                    (timestamp, BaseType::TimestampTz),
                    (time_zone, BaseType::Text),
                ] {
                    if infer_parameter_expression_type(argument, schema, types).is_none() {
                        constrain_parameter_type(argument, Some(default), types)?;
                    }
                }
                Ok(())
            })(),
            ast::Expr::Between {
                expr,
                negated,
                low,
                high,
            } => infer_expression_parameters(
                &executor::expand_between_expression(expr, low, high, *negated),
                schema,
                expected,
                types,
            ),
            ast::Expr::InList { expr, list, .. } => (|| {
                let left = match expr.as_ref() {
                    ast::Expr::Tuple(fields) => fields.as_slice(),
                    expr => std::slice::from_ref(expr),
                };
                for candidate in list {
                    let right = match candidate {
                        ast::Expr::Tuple(fields) => fields.as_slice(),
                        candidate => std::slice::from_ref(candidate),
                    };
                    if left.len() != right.len() {
                        return Err(PgError::create(
                            SqlState::SyntaxError,
                            "subquery has too many columns",
                        ));
                    }
                    for (left, right) in left.iter().zip(right) {
                        constrain_parameter_type(
                            left,
                            executor::infer_expression_type(right, schema).ok(),
                            types,
                        )?;
                        constrain_parameter_type(
                            right,
                            executor::infer_expression_type(left, schema).ok(),
                            types,
                        )?;
                    }
                }
                Ok(())
            })(),
            ast::Expr::AnyOp { left, right, .. } | ast::Expr::AllOp { left, right, .. } => {
                let left_type = infer_parameter_expression_type(left, schema, types);
                let right_type = infer_parameter_expression_type(right, schema, types);
                if let Some(element_type) = right_type.and_then(BaseType::get_array_element_type) {
                    constrain_parameter_type(left, Some(element_type), types)
                } else if let Some(array_type) = left_type.and_then(BaseType::get_array_type) {
                    constrain_parameter_type(right, Some(array_type), types)
                } else if executor::is_parameter_placeholder(right) {
                    reject_unsupported("quantified array parameter type is not implemented")
                } else {
                    constrain_parameter_type(left, right_type, types)
                        .and_then(|()| constrain_parameter_type(right, left_type, types))
                }
            }
            ast::Expr::IsTrue(inner)
            | ast::Expr::IsFalse(inner)
            | ast::Expr::IsUnknown(inner)
            | ast::Expr::IsNotTrue(inner)
            | ast::Expr::IsNotFalse(inner)
            | ast::Expr::IsNotUnknown(inner) => {
                constrain_parameter_type(inner, Some(BaseType::Bool), types)
            }
            ast::Expr::Function(function) => infer_function_parameters(function, schema, types),
            _ => Ok(()),
        };
        if let Err(infer_error) = result {
            error = Some(infer_error);
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    error.map_or(Ok(()), Err)
}

fn infer_parameter_expression_type(
    expression: &ast::Expr,
    scope: executor::RowScope<'_>,
    types: &[Option<BaseType>],
) -> Option<BaseType> {
    match expression {
        ast::Expr::Nested(inner) => infer_parameter_expression_type(inner, scope, types),
        ast::Expr::Value(value) => match &value.value {
            ast::Value::Placeholder(placeholder) => {
                types[parse_placeholder_index(placeholder).expect("parameter index was validated")]
            }
            _ => executor::infer_expression_type(expression, scope).ok(),
        },
        _ => executor::infer_expression_type(expression, scope).ok(),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn infer_function_parameters(
    function: &ast::Function,
    schema: executor::RowScope<'_>,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    let ast::FunctionArguments::List(list) = &function.args else {
        return Ok(());
    };
    let arguments = list
        .args
        .iter()
        .filter_map(|argument| match argument {
            ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expression)) => Some(expression),
            _ => None,
        })
        .collect::<Vec<_>>();
    let name = executor::normalize_function_name(&function.name)?;
    let argument_types = arguments
        .iter()
        .map(|argument| {
            if executor::is_null_literal(argument)
                || executor::extract_unknown_string_literal(argument).is_some()
            {
                None
            } else {
                infer_parameter_expression_type(argument, schema, types)
            }
        })
        .collect::<Vec<_>>();
    if let Some(ast::WindowType::WindowSpec(window)) = &function.over
        && let Some(frame) = &window.window_frame
    {
        let expected = match frame.units {
            ast::WindowFrameUnits::Rows | ast::WindowFrameUnits::Groups => Some(BaseType::Int8),
            ast::WindowFrameUnits::Range => window.order_by.first().and_then(|order| {
                match executor::infer_expression_type(&order.expr, schema).ok()? {
                    BaseType::Date
                    | BaseType::Time
                    | BaseType::Timestamp
                    | BaseType::TimestampTz
                    | BaseType::Interval => Some(BaseType::Interval),
                    data_type => Some(data_type),
                }
            }),
        };
        for bound in [
            &frame.start_bound,
            frame
                .end_bound
                .as_ref()
                .unwrap_or(&ast::WindowFrameBound::CurrentRow),
        ] {
            if let ast::WindowFrameBound::Preceding(Some(offset))
            | ast::WindowFrameBound::Following(Some(offset)) = bound
            {
                constrain_parameter_type(offset, expected, types)?;
            }
        }
    }
    if let Some(signature) = executor::resolve_runtime_function(&name, &argument_types) {
        let (targets, _) = signature?;
        for ((argument, argument_type), target) in arguments.iter().zip(argument_types).zip(targets)
        {
            if argument_type.is_none() {
                constrain_parameter_type(argument, Some(target), types)?;
            }
        }
        return Ok(());
    }
    if let Some(targets) = executor::resolve_json_function_arguments(&name) {
        for (argument, target) in arguments.iter().zip(targets) {
            constrain_parameter_type(argument, Some(target), types)?;
        }
        return Ok(());
    }
    if matches!(
        name.as_str(),
        "json_build_object"
            | "jsonb_build_object"
            | "json_build_array"
            | "jsonb_build_array"
            | "to_json"
            | "to_jsonb"
    ) {
        return Ok(());
    }
    if matches!(name.as_str(), "nextval" | "currval") {
        for argument in arguments {
            constrain_parameter_type(argument, Some(BaseType::Text), types)?;
        }
        return Ok(());
    }
    if name == "setval" {
        for (argument, expected) in
            arguments
                .iter()
                .zip([BaseType::Text, BaseType::Int8, BaseType::Bool])
        {
            constrain_parameter_type(argument, Some(expected), types)?;
        }
        return Ok(());
    }
    if name == "current_setting" {
        for (argument, expected) in arguments.iter().zip([BaseType::Text, BaseType::Bool]) {
            constrain_parameter_type(argument, Some(expected), types)?;
        }
        return Ok(());
    }
    if name == "set_config" {
        for (argument, expected) in
            arguments
                .iter()
                .zip([BaseType::Text, BaseType::Text, BaseType::Bool])
        {
            constrain_parameter_type(argument, Some(expected), types)?;
        }
        return Ok(());
    }
    if matches!(name.as_str(), "lag" | "lead") && function.over.is_some() {
        if let Some(offset) = arguments.get(1) {
            constrain_parameter_type(offset, Some(BaseType::Int4), types)?;
        }
        if let Some(default) = arguments.get(2) {
            let value_type = infer_parameter_expression_type(arguments[0], schema, types)
                .or_else(|| infer_parameter_expression_type(default, schema, types));
            constrain_parameter_type(arguments[0], value_type, types)?;
            constrain_parameter_type(default, value_type, types)?;
        }
        return Ok(());
    }
    if name == "nth_value" && function.over.is_some() {
        if let Some(offset) = arguments.get(1) {
            constrain_parameter_type(offset, Some(BaseType::Int4), types)?;
        }
        return Ok(());
    }
    let expected = match name.as_str() {
        "length" | "lower" | "upper" | "btrim" | "string_agg" => Some(BaseType::Text),
        _ => arguments
            .iter()
            .find_map(|argument| executor::infer_expression_type(argument, schema).ok()),
    };
    for argument in arguments {
        constrain_parameter_type(argument, expected, types)?;
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn constrain_parameter_type(
    expression: &ast::Expr,
    expected: Option<BaseType>,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let mut expression = expression;
    while let ast::Expr::Nested(inner) = expression {
        expression = inner;
    }
    let ast::Expr::Value(value) = expression else {
        return Ok(());
    };
    let ast::Value::Placeholder(placeholder) = &value.value else {
        return Ok(());
    };
    let index = parse_placeholder_index(placeholder)?;
    let slot = &mut types[index];
    if let Some(previous) = *slot
        && previous != expected
    {
        let Some(common) = coercion::resolve_common_type(previous, expected) else {
            return Err(PgError::create(
                SqlState::AmbiguousParameter,
                format!("inconsistent types deduced for parameter {placeholder}"),
            ));
        };
        *slot = Some(common);
    } else {
        *slot = Some(expected);
    }
    Ok(())
}
