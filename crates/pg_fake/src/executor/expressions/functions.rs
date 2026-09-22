use super::{
    comparisons::{evaluate_comparison, validate_equality_type, validate_ordering_type},
    evaluate, evaluate_and_coerce,
    literals::{extract_unknown_string_literal, is_null_literal},
    types::{infer_expression_type, is_numeric_type, resolve_expression_list_type},
};
use crate::executor::{
    StatementContext,
    aggregates::{infer_aggregate_return_type, is_aggregate_function},
    json, normalize_function_name,
    scope::RowScope,
};
use crate::{
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, Value},
};
use bigdecimal::{BigDecimal, num_bigint::BigInt};
use rand_chacha::rand_core::RngCore;
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn extract_function_arguments(function: &ast::Function) -> Result<Vec<&ast::Expr>> {
    if function.uses_odbc_syntax
        || !matches!(function.parameters, ast::FunctionArguments::None)
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || function.over.is_some()
        || !function.within_group.is_empty()
    {
        return reject_unsupported("function feature is not implemented");
    }
    let arguments = match &function.args {
        ast::FunctionArguments::None => return Ok(Vec::new()),
        ast::FunctionArguments::List(arguments) => arguments,
        ast::FunctionArguments::Subquery(_) => {
            return Err(PgError::create(
                SqlState::UndefinedFunction,
                "function signature does not exist",
            ));
        }
    };
    if arguments.duplicate_treatment.is_some() || !arguments.clauses.is_empty() {
        return reject_unsupported("function argument feature is not implemented");
    }
    arguments
        .args
        .iter()
        .map(|argument| match argument {
            ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expression)) => Ok(expression),
            _ => reject_unsupported("function argument is not implemented"),
        })
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn infer_window_return_type(
    function: &ast::Function,
    schema: RowScope<'_>,
) -> Result<Option<BaseType>> {
    let Some(ast::WindowType::WindowSpec(window)) = &function.over else {
        if function.over.is_some() {
            return reject_unsupported("named windows are not implemented");
        }
        return Ok(None);
    };
    let name = normalize_function_name(&function.name)?;
    if function.null_treatment.is_some() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "syntax error at or near NULLS",
        ));
    }
    if function.uses_odbc_syntax
        || !matches!(function.parameters, ast::FunctionArguments::None)
        || !function.within_group.is_empty()
    {
        return reject_unsupported("window function feature is not implemented");
    }
    validate_window_frame(window, schema)?;
    if matches!(
        name.as_str(),
        "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
            | "bool_and"
            | "bool_or"
            | "string_agg"
            | "array_agg"
    ) {
        let ast::FunctionArguments::List(arguments) = &function.args else {
            return Err(PgError::create(
                SqlState::UndefinedFunction,
                format!("function {name} does not exist"),
            ));
        };
        if arguments.duplicate_treatment == Some(ast::DuplicateTreatment::Distinct) {
            return reject_unsupported("DISTINCT is not implemented for window functions");
        }
        if !arguments.clauses.is_empty() {
            return reject_unsupported(
                "aggregate ORDER BY is not implemented for window functions",
            );
        }
        validate_window_partition_and_order(window, schema)?;
        let mut aggregate = function.clone();
        aggregate.over = None;
        let call = crate::executor::aggregates::parse_aggregate_call(&aggregate, schema)?;
        return Ok(Some(call.descriptor.get_result_type()));
    }
    match name.as_str() {
        "row_number" | "rank" | "dense_rank" | "percent_rank" | "cume_dist" => {
            let ast::FunctionArguments::List(arguments) = &function.args else {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    format!("function {name} does not exist"),
                ));
            };
            if !arguments.args.is_empty()
                || !arguments.clauses.is_empty()
                || arguments.duplicate_treatment.is_some()
            {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    format!("function {name} does not exist"),
                ));
            }
            for partition in &window.partition_by {
                validate_equality_type(infer_expression_type(partition, schema)?)?;
            }
            for order in &window.order_by {
                if order.with_fill.is_some()
                    || matches!(order.options.sort, Some(ast::OrderBySort::Using(_)))
                {
                    return reject_unsupported("window order feature is not implemented");
                }
                validate_ordering_type(infer_expression_type(&order.expr, schema)?)?;
            }
            Ok(Some(
                if matches!(name.as_str(), "percent_rank" | "cume_dist") {
                    BaseType::Float8
                } else {
                    BaseType::Int8
                },
            ))
        }
        "ntile" => {
            let ast::FunctionArguments::List(arguments) = &function.args else {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    "function ntile does not exist",
                ));
            };
            if !matches!(
                arguments.args.as_slice(),
                [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(_))]
            ) || !arguments.clauses.is_empty()
                || arguments.duplicate_treatment.is_some()
            {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    "function ntile does not exist",
                ));
            }
            validate_function_argument(
                match &arguments.args[0] {
                    ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expression)) => expression,
                    _ => unreachable!("ntile argument was validated"),
                },
                BaseType::Int4,
                schema,
                &|| PgError::create(SqlState::UndefinedFunction, "function ntile does not exist"),
            )?;
            for partition in &window.partition_by {
                validate_equality_type(infer_expression_type(partition, schema)?)?;
            }
            for order in &window.order_by {
                if order.with_fill.is_some()
                    || matches!(order.options.sort, Some(ast::OrderBySort::Using(_)))
                {
                    return reject_unsupported("window order feature is not implemented");
                }
                validate_ordering_type(infer_expression_type(&order.expr, schema)?)?;
            }
            Ok(Some(BaseType::Int4))
        }
        "lag" | "lead" => {
            let ast::FunctionArguments::List(arguments) = &function.args else {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    format!("function {name} does not exist"),
                ));
            };
            let expressions = arguments
                .args
                .iter()
                .map(|argument| match argument {
                    ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expression)) => {
                        Ok(expression)
                    }
                    _ => Err(PgError::create(
                        SqlState::UndefinedFunction,
                        format!("function {name} does not exist"),
                    )),
                })
                .collect::<Result<Vec<_>>>()?;
            if !(1..=3).contains(&expressions.len())
                || !arguments.clauses.is_empty()
                || arguments.duplicate_treatment.is_some()
            {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    format!("function {name} does not exist"),
                ));
            }
            if let Some(offset) = expressions.get(1) {
                validate_function_argument(offset, BaseType::Int4, schema, &|| {
                    PgError::create(
                        SqlState::UndefinedFunction,
                        format!("function {name} does not exist"),
                    )
                })?;
            }
            validate_window_partition_and_order(window, schema)?;
            let data_type = resolve_expression_list_type(
                &[
                    expressions[0],
                    expressions.get(2).copied().unwrap_or(expressions[0]),
                ],
                schema,
            )?;
            if let Some(text) = expressions
                .get(2)
                .and_then(|default| extract_unknown_string_literal(default))
            {
                coercion::coerce_unknown(
                    text,
                    crate::value::PgType::create(data_type),
                    CastContext::Implicit,
                    "UTC",
                )?;
            }
            Ok(Some(data_type))
        }
        "first_value" | "last_value" => {
            let ast::FunctionArguments::List(arguments) = &function.args else {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    format!("function {name} does not exist"),
                ));
            };
            let [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expression))] =
                arguments.args.as_slice()
            else {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    format!("function {name} does not exist"),
                ));
            };
            if !arguments.clauses.is_empty() || arguments.duplicate_treatment.is_some() {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    format!("function {name} does not exist"),
                ));
            }
            validate_window_partition_and_order(window, schema)?;
            Ok(Some(infer_expression_type(expression, schema)?))
        }
        "nth_value" => {
            let ast::FunctionArguments::List(arguments) = &function.args else {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    "function nth_value does not exist",
                ));
            };
            let [
                ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expression)),
                ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(offset)),
            ] = arguments.args.as_slice()
            else {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    "function nth_value does not exist",
                ));
            };
            if !arguments.clauses.is_empty() || arguments.duplicate_treatment.is_some() {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    "function nth_value does not exist",
                ));
            }
            validate_function_argument(offset, BaseType::Int4, schema, &|| {
                PgError::create(
                    SqlState::UndefinedFunction,
                    "function nth_value does not exist",
                )
            })?;
            validate_window_partition_and_order(window, schema)?;
            Ok(Some(infer_expression_type(expression, schema)?))
        }
        _ => Err(PgError::create(
            SqlState::UndefinedFunction,
            format!("function {name} does not exist"),
        )),
    }
}

fn validate_window_frame(window: &ast::WindowSpec, schema: RowScope<'_>) -> Result<()> {
    let Some(frame) = &window.window_frame else {
        return Ok(());
    };
    let end = frame
        .end_bound
        .as_ref()
        .unwrap_or(&ast::WindowFrameBound::CurrentRow);
    if matches!(frame.start_bound, ast::WindowFrameBound::Following(None)) {
        return Err(PgError::create(
            SqlState::WindowingError,
            "frame start cannot be UNBOUNDED FOLLOWING",
        ));
    }
    if matches!(end, ast::WindowFrameBound::Preceding(None)) {
        return Err(PgError::create(
            SqlState::WindowingError,
            "frame end cannot be UNBOUNDED PRECEDING",
        ));
    }
    if matches!(frame.start_bound, ast::WindowFrameBound::CurrentRow)
        && matches!(end, ast::WindowFrameBound::Preceding(Some(_)))
        || matches!(frame.start_bound, ast::WindowFrameBound::Following(Some(_)))
            && matches!(
                end,
                ast::WindowFrameBound::CurrentRow | ast::WindowFrameBound::Preceding(Some(_))
            )
    {
        return Err(PgError::create(
            SqlState::WindowingError,
            "frame starting bound must not follow frame ending bound",
        ));
    }
    let offsets = [&frame.start_bound, end]
        .into_iter()
        .filter_map(|bound| match bound {
            ast::WindowFrameBound::Preceding(Some(offset))
            | ast::WindowFrameBound::Following(Some(offset)) => Some(offset.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    struct OffsetInspector<'a> {
        query_depth: usize,
        error: Option<PgError>,
        schema: RowScope<'a>,
    }
    impl ast::Visitor for OffsetInspector<'_> {
        type Break = ();

        fn pre_visit_query(&mut self, _query: &ast::Query) -> std::ops::ControlFlow<Self::Break> {
            self.query_depth += 1;
            std::ops::ControlFlow::Continue(())
        }

        fn post_visit_query(&mut self, _query: &ast::Query) -> std::ops::ControlFlow<Self::Break> {
            self.query_depth -= 1;
            std::ops::ControlFlow::Continue(())
        }

        fn pre_visit_expr(&mut self, expression: &ast::Expr) -> std::ops::ControlFlow<Self::Break> {
            self.error = match expression {
                ast::Expr::Identifier(identifier)
                    if self.query_depth == 0
                        || self
                            .schema
                            .resolve_column(std::slice::from_ref(identifier))
                            .is_ok() =>
                {
                    Some(PgError::create(
                        SqlState::InvalidColumnReference,
                        "argument of window frame must not contain variables",
                    ))
                }
                ast::Expr::CompoundIdentifier(identifiers)
                    if self.query_depth == 0 || self.schema.resolve_column(identifiers).is_ok() =>
                {
                    Some(PgError::create(
                        SqlState::InvalidColumnReference,
                        "argument of window frame must not contain variables",
                    ))
                }
                ast::Expr::Function(function) if function.over.is_some() => Some(PgError::create(
                    SqlState::WindowingError,
                    "window functions are not allowed in window definitions",
                )),
                ast::Expr::Function(function)
                    if crate::executor::aggregates::is_aggregate_function(function) =>
                {
                    Some(PgError::create(
                        SqlState::GroupingError,
                        "aggregate functions are not allowed in window definitions",
                    ))
                }
                _ => None,
            };
            if self.error.is_some() {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        }
    }
    for offset in &offsets {
        let mut inspector = OffsetInspector {
            query_depth: 0,
            error: None,
            schema,
        };
        let _ = ast::Visit::visit(*offset, &mut inspector);
        if let Some(error) = inspector.error {
            return Err(error);
        }
    }
    if !offsets.is_empty()
        && matches!(
            frame.units,
            ast::WindowFrameUnits::Range | ast::WindowFrameUnits::Groups
        )
        && window.order_by.len() != 1
    {
        return Err(PgError::create(
            SqlState::WindowingError,
            format!(
                "{} with offset PRECEDING/FOLLOWING requires exactly one ORDER BY column",
                frame.units
            ),
        ));
    }
    if matches!(
        frame.units,
        ast::WindowFrameUnits::Rows | ast::WindowFrameUnits::Groups
    ) {
        for offset in offsets {
            validate_function_argument(offset, BaseType::Int8, schema, &|| {
                PgError::create(
                    SqlState::DatatypeMismatch,
                    "frame offset has incompatible type",
                )
            })?;
        }
    } else {
        let order_type = infer_expression_type(&window.order_by[0].expr, schema)?;
        let offset_type = if is_numeric_type(order_type) {
            order_type
        } else if matches!(
            order_type,
            BaseType::Date
                | BaseType::Time
                | BaseType::Timestamp
                | BaseType::TimestampTz
                | BaseType::Interval
        ) {
            BaseType::Interval
        } else {
            return reject_unsupported(format!(
                "RANGE with offset PRECEDING/FOLLOWING is not supported for type {order_type:?}",
            ));
        };
        for offset in offsets {
            validate_function_argument(offset, offset_type, schema, &|| {
                PgError::create(
                    SqlState::DatatypeMismatch,
                    "RANGE offset has incompatible type",
                )
            })?;
        }
    }
    Ok(())
}

fn validate_window_partition_and_order(
    window: &ast::WindowSpec,
    schema: RowScope<'_>,
) -> Result<()> {
    for partition in &window.partition_by {
        validate_equality_type(infer_expression_type(partition, schema)?)?;
    }
    for order in &window.order_by {
        if order.with_fill.is_some()
            || matches!(order.options.sort, Some(ast::OrderBySort::Using(_)))
        {
            return reject_unsupported("window order feature is not implemented");
        }
        validate_ordering_type(infer_expression_type(&order.expr, schema)?)?;
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn infer_function_return_type(
    function: &ast::Function,
    schema: RowScope<'_>,
) -> Result<BaseType> {
    if let Some(result) = infer_window_return_type(function, schema)? {
        return Ok(result);
    }
    if let Some(result) = infer_aggregate_return_type(function, schema)? {
        return Ok(result);
    }
    let function_name = normalize_function_name(&function.name)?;
    let arguments = extract_function_arguments(function)?;
    let signature_error = || {
        PgError::create(
            SqlState::UndefinedFunction,
            format!("function {function_name} does not exist"),
        )
    };
    if let Some(base) = json::infer_json_function(&function_name, &arguments, schema)? {
        return Ok(base);
    }
    if let Some((_, result)) =
        super::runtime::infer_runtime_function(&function_name, &arguments, schema)?
    {
        return Ok(result);
    }
    match function_name.as_str() {
        "coalesce" if !arguments.is_empty() => resolve_expression_list_type(&arguments, schema),
        "greatest" | "least" if !arguments.is_empty() => {
            let data_type = resolve_expression_list_type(&arguments, schema)?;
            validate_ordering_type(data_type)?;
            Ok(data_type)
        }
        "nullif" if arguments.len() == 2 => {
            let data_type = resolve_expression_list_type(&arguments, schema)?;
            validate_equality_type(data_type)?;
            Ok(data_type)
        }
        "length" | "lower" | "upper" | "btrim" if arguments.len() == 1 => {
            let base = infer_expression_type(arguments[0], schema)?;
            if !is_null_literal(arguments[0])
                && !matches!(base, BaseType::Text | BaseType::Varchar | BaseType::Bpchar)
            {
                return Err(signature_error());
            }
            Ok(if function_name == "length" {
                BaseType::Int4
            } else {
                BaseType::Text
            })
        }
        "abs" if arguments.len() == 1 => {
            if extract_unknown_string_literal(arguments[0]).is_some() {
                return Ok(BaseType::Float8);
            }
            let base = infer_expression_type(arguments[0], schema)?;
            if !is_null_literal(arguments[0]) && !is_numeric_type(base) {
                return Err(signature_error());
            }
            Ok(base)
        }
        "gen_random_uuid" | "uuidv4" | "uuidv7" if arguments.is_empty() => Ok(BaseType::Uuid),
        "pg_is_in_recovery" if arguments.is_empty() => Ok(BaseType::Bool),
        "current_setting" if matches!(arguments.len(), 1 | 2) => {
            validate_function_argument(arguments[0], BaseType::Text, schema, &signature_error)?;
            if let Some(missing_ok) = arguments.get(1) {
                validate_function_argument(missing_ok, BaseType::Bool, schema, &signature_error)?;
            }
            Ok(BaseType::Text)
        }
        "set_config" if arguments.len() == 3 => {
            validate_function_argument(arguments[0], BaseType::Text, schema, &signature_error)?;
            validate_function_argument(arguments[1], BaseType::Text, schema, &signature_error)?;
            validate_function_argument(arguments[2], BaseType::Bool, schema, &signature_error)?;
            Ok(BaseType::Text)
        }
        "now"
        | "current_timestamp"
        | "transaction_timestamp"
        | "statement_timestamp"
        | "clock_timestamp"
            if arguments.is_empty() =>
        {
            Ok(BaseType::TimestampTz)
        }
        "nextval" | "currval" if arguments.len() == 1 => {
            validate_function_argument(arguments[0], BaseType::Text, schema, &signature_error)?;
            Ok(BaseType::Int8)
        }
        "pg_get_serial_sequence" if arguments.len() == 2 => {
            validate_function_argument(arguments[0], BaseType::Text, schema, &signature_error)?;
            validate_function_argument(arguments[1], BaseType::Text, schema, &signature_error)?;
            Ok(BaseType::Text)
        }
        "to_regclass" if arguments.len() == 1 => {
            validate_function_argument(arguments[0], BaseType::Text, schema, &signature_error)?;
            Ok(BaseType::Regclass)
        }
        "format_type" if arguments.len() == 2 => {
            let oid = infer_expression_type(arguments[0], schema)?;
            let typmod = infer_expression_type(arguments[1], schema)?;
            if !matches!(
                oid,
                BaseType::Oid | BaseType::Int4 | BaseType::Int8 | BaseType::Regclass
            ) || typmod != BaseType::Int4
            {
                return Err(signature_error());
            }
            Ok(BaseType::Text)
        }
        "lastval" if arguments.is_empty() => Ok(BaseType::Int8),
        "setval" if matches!(arguments.len(), 2 | 3) => {
            validate_function_argument(arguments[0], BaseType::Text, schema, &signature_error)?;
            validate_function_argument(arguments[1], BaseType::Int8, schema, &signature_error)?;
            if let Some(is_called) = arguments.get(2) {
                validate_function_argument(is_called, BaseType::Bool, schema, &signature_error)?;
            }
            Ok(BaseType::Int8)
        }
        "coalesce"
        | "nullif"
        | "greatest"
        | "least"
        | "length"
        | "lower"
        | "upper"
        | "btrim"
        | "abs"
        | "nextval"
        | "currval"
        | "lastval"
        | "setval"
        | "pg_get_serial_sequence"
        | "pg_is_in_recovery"
        | "current_setting"
        | "set_config"
        | "to_regclass"
        | "format_type" => Err(signature_error()),
        _ => Err(PgError::create(
            SqlState::UndefinedFunction,
            format!("function {function_name} does not exist"),
        )),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn validate_function_argument(
    argument: &ast::Expr,
    target: BaseType,
    schema: RowScope<'_>,
    create_error: &impl Fn() -> PgError,
) -> Result<()> {
    if is_null_literal(argument) {
        return Ok(());
    }
    if let Some(text) = extract_unknown_string_literal(argument) {
        coercion::coerce_unknown(
            text,
            crate::value::PgType::create(target),
            CastContext::Implicit,
            "UTC",
        )?;
        return Ok(());
    }
    let source = infer_expression_type(argument, schema)?;
    if coercion::can_cast(source, target, CastContext::Implicit) {
        Ok(())
    } else {
        Err(create_error())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_function(
    function: &ast::Function,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    if infer_window_return_type(function, schema)?.is_some() {
        return Err(PgError::create(
            SqlState::WindowingError,
            "window function is not allowed in this context",
        ));
    }
    if is_aggregate_function(function) {
        infer_aggregate_return_type(function, schema)?;
        return Err(PgError::create(
            SqlState::GroupingError,
            "aggregate function is not allowed in this context",
        ));
    }
    infer_function_return_type(function, schema)?;
    let function_name = normalize_function_name(&function.name)?;
    let arguments = extract_function_arguments(function)?;
    let result_type = infer_function_return_type(function, schema)?;
    if json::infer_json_function(&function_name, &arguments, schema)?.is_some() {
        return json::evaluate_json_function(
            &function_name,
            &arguments,
            result_type,
            schema,
            row,
            context,
        );
    }
    if let Some((targets, _)) =
        super::runtime::infer_runtime_function(&function_name, &arguments, schema)?
    {
        return super::runtime::evaluate_runtime_function(
            &function_name,
            &arguments,
            &targets,
            schema,
            row,
            context,
        );
    }
    match function_name.as_str() {
        "pg_is_in_recovery" => Ok(Value::Bool(false)),
        "current_setting" => {
            let name = evaluate_and_coerce(
                arguments[0],
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let missing_ok = if let Some(argument) = arguments.get(1) {
                evaluate_and_coerce(
                    argument,
                    BaseType::Bool,
                    CastContext::Implicit,
                    schema,
                    row,
                    context,
                )?
            } else {
                Value::Bool(false)
            };
            let (Value::Text(name), Value::Bool(missing_ok)) = (name, missing_ok) else {
                return Ok(Value::Null);
            };
            Ok(context
                .guc
                .lock()
                .expect("GUC context mutex is poisoned")
                .get_setting(&name, missing_ok)?
                .map(Value::Text)
                .unwrap_or(Value::Null))
        }
        "set_config" => {
            let name = evaluate_and_coerce(
                arguments[0],
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let value = evaluate_and_coerce(
                arguments[1],
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let local = evaluate_and_coerce(
                arguments[2],
                BaseType::Bool,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let name = match name {
                Value::Text(name) => name,
                Value::Null => {
                    return Err(PgError::create(
                        SqlState::NullValueNotAllowed,
                        "SET requires parameter name",
                    ));
                }
                _ => unreachable!("set_config name is coerced to text"),
            };
            let value = match value {
                Value::Text(value) => Some(value),
                Value::Null => None,
                _ => unreachable!("set_config value is coerced to text"),
            };
            let local = match local {
                Value::Bool(local) => local,
                Value::Null => false,
                _ => unreachable!("set_config local flag is coerced to boolean"),
            };
            let value = context
                .guc
                .lock()
                .expect("GUC context mutex is poisoned")
                .set_setting(&name, value.as_deref(), local)?;
            Ok(Value::Text(value))
        }
        "to_regclass" => {
            let name = evaluate_and_coerce(
                arguments[0],
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let Value::Text(name) = name else {
                return Ok(Value::Null);
            };
            Ok(context
                .sequences
                .resolve_regclass_lenient(&name)?
                .map(Value::Regclass)
                .unwrap_or(Value::Null))
        }
        "format_type" => {
            let oid = evaluate(arguments[0], schema, row, context)?;
            let typmod = evaluate_and_coerce(
                arguments[1],
                BaseType::Int4,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let oid = match oid {
                Value::Oid(oid) => oid,
                Value::Int4(oid) => oid as u32,
                Value::Int8(oid) => u32::try_from(oid).map_err(|_| {
                    PgError::create(SqlState::NumericValueOutOfRange, "OID out of range")
                })?,
                Value::Regclass(crate::value::PgRegclass(oid)) => oid,
                Value::Null => return Ok(Value::Null),
                _ => unreachable!("format_type arguments were type-checked"),
            };
            let Value::Int4(typmod) = typmod else {
                return Ok(Value::Null);
            };
            Ok(Value::Text(super::super::format_type(oid, typmod)?))
        }
        "gen_random_uuid" | "uuidv4" => {
            let mut bytes = [0; 16];
            context
                .rng
                .lock()
                .expect("rng mutex is poisoned")
                .fill_bytes(&mut bytes);
            Ok(Value::Uuid(
                uuid::Builder::from_random_bytes(bytes).into_uuid(),
            ))
        }
        "uuidv7" => {
            let milliseconds =
                u64::try_from(context.clock_timestamp.timestamp_millis()).map_err(|_| {
                    PgError::create(
                        SqlState::NumericValueOutOfRange,
                        "uuidv7 timestamp is out of range",
                    )
                })?;
            let mut bytes = [0; 10];
            context
                .rng
                .lock()
                .expect("rng mutex is poisoned")
                .fill_bytes(&mut bytes);
            Ok(Value::Uuid(
                uuid::Builder::from_unix_timestamp_millis(milliseconds, &bytes).into_uuid(),
            ))
        }
        "now"
        | "current_timestamp"
        | "transaction_timestamp"
        | "statement_timestamp"
        | "clock_timestamp" => {
            let value = match function_name.as_str() {
                "now" | "current_timestamp" | "transaction_timestamp" => {
                    context.transaction_timestamp
                }
                "statement_timestamp" => context.statement_timestamp,
                "clock_timestamp" => context.clock_timestamp,
                _ => unreachable!(),
            };
            Ok(Value::TimestampTz(crate::value::PgTimestampTz::Finite(
                value,
            )))
        }
        "nextval" | "currval" => {
            let name = evaluate_and_coerce(
                arguments[0],
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let Value::Text(name) = name else {
                return Ok(Value::Null);
            };
            let value = if function_name == "nextval" {
                context.sequences.get_next_value(&name)?
            } else {
                context.sequences.get_current_value(&name)?
            };
            Ok(Value::Int8(value))
        }
        "lastval" => Ok(Value::Int8(context.sequences.get_last_value()?)),
        "pg_get_serial_sequence" => {
            let table = evaluate_and_coerce(
                arguments[0],
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let column = evaluate_and_coerce(
                arguments[1],
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let (Value::Text(table), Value::Text(column)) = (table, column) else {
                return Ok(Value::Null);
            };
            Ok(context
                .sequences
                .get_owned_sequence(&table, &column)?
                .map(Value::Text)
                .unwrap_or(Value::Null))
        }
        "setval" => {
            let name = evaluate_and_coerce(
                arguments[0],
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let value = evaluate_and_coerce(
                arguments[1],
                BaseType::Int8,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let is_called = if let Some(argument) = arguments.get(2) {
                evaluate_and_coerce(
                    argument,
                    BaseType::Bool,
                    CastContext::Implicit,
                    schema,
                    row,
                    context,
                )?
            } else {
                Value::Bool(true)
            };
            let (Value::Text(name), Value::Int8(value), Value::Bool(is_called)) =
                (name, value, is_called)
            else {
                return Ok(Value::Null);
            };
            Ok(Value::Int8(
                context.sequences.set_value(&name, value, is_called)?,
            ))
        }
        "coalesce" => {
            for argument in arguments {
                let value = evaluate_and_coerce(
                    argument,
                    result_type,
                    CastContext::Implicit,
                    schema,
                    row,
                    context,
                )?;
                if !value.is_null() {
                    return Ok(value);
                }
            }
            Ok(Value::Null)
        }
        "nullif" => {
            let left = evaluate_and_coerce(
                arguments[0],
                result_type,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            if left.is_null() {
                return Ok(Value::Null);
            }
            let right = evaluate_and_coerce(
                arguments[1],
                result_type,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            if !right.is_null()
                && matches!(
                    evaluate_comparison(&ast::BinaryOperator::Eq, &left, &right)?,
                    Value::Bool(true)
                )
            {
                Ok(Value::Null)
            } else {
                Ok(left)
            }
        }
        "greatest" | "least" => {
            let mut selected = None;
            for argument in arguments {
                let value = evaluate_and_coerce(
                    argument,
                    result_type,
                    CastContext::Implicit,
                    schema,
                    row,
                    context,
                )?;
                if value.is_null() {
                    continue;
                }
                selected = Some(match selected {
                    None => value,
                    Some(current) => {
                        let operator = if function_name == "greatest" {
                            ast::BinaryOperator::Gt
                        } else {
                            ast::BinaryOperator::Lt
                        };
                        if matches!(
                            evaluate_comparison(&operator, &value, &current)?,
                            Value::Bool(true)
                        ) {
                            value
                        } else {
                            current
                        }
                    }
                });
            }
            Ok(selected.unwrap_or(Value::Null))
        }
        "length" => match evaluate(arguments[0], schema, row, context)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => {
                let value = if infer_expression_type(arguments[0], schema)? == BaseType::Bpchar {
                    value.trim_end_matches(' ')
                } else {
                    &value
                };
                Ok(Value::Int4(
                    i32::try_from(value.chars().count()).expect("text length must fit in int4"),
                ))
            }
            _ => unreachable!("length argument was type-checked"),
        },
        "lower" => match evaluate(arguments[0], schema, row, context)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => {
                let value = if infer_expression_type(arguments[0], schema)? == BaseType::Bpchar {
                    value.trim_end_matches(' ')
                } else {
                    &value
                };
                Ok(Value::Text(value.to_lowercase()))
            }
            _ => unreachable!("lower argument was type-checked"),
        },
        "upper" => match evaluate(arguments[0], schema, row, context)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => {
                let value = if infer_expression_type(arguments[0], schema)? == BaseType::Bpchar {
                    value.trim_end_matches(' ')
                } else {
                    &value
                };
                Ok(Value::Text(value.to_uppercase()))
            }
            _ => unreachable!("upper argument was type-checked"),
        },
        "btrim" => match evaluate(arguments[0], schema, row, context)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => Ok(Value::Text(value.trim_matches(' ').into())),
            _ => unreachable!("btrim argument was type-checked"),
        },
        "abs" => match evaluate(arguments[0], schema, row, context)? {
            Value::Null => Ok(Value::Null),
            Value::Int2(value) => value.checked_abs().map(Value::Int2).ok_or_else(|| {
                PgError::create(SqlState::NumericValueOutOfRange, "smallint out of range")
            }),
            Value::Int4(value) => value.checked_abs().map(Value::Int4).ok_or_else(|| {
                PgError::create(SqlState::NumericValueOutOfRange, "integer out of range")
            }),
            Value::Int8(value) => value.checked_abs().map(Value::Int8).ok_or_else(|| {
                PgError::create(SqlState::NumericValueOutOfRange, "bigint out of range")
            }),
            Value::Float4(value) => Ok(Value::Float4(value.abs())),
            Value::Float8(value) => Ok(Value::Float8(value.abs())),
            Value::Numeric(value) => Ok(Value::Numeric(value.abs())),
            _ => unreachable!("abs argument was type-checked"),
        },
        _ => unreachable!("function name was type-checked"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn extract_datetime_field(field: ast::DateTimeField, value: Value) -> Result<Value> {
    use chrono::{Datelike, Timelike};
    let value = match value {
        Value::Null => return Ok(Value::Null),
        Value::Date(crate::value::PgDate::Finite(value)) => match field {
            ast::DateTimeField::Year => value.year() as i64,
            ast::DateTimeField::Month => i64::from(value.month()),
            ast::DateTimeField::Day => i64::from(value.day()),
            ast::DateTimeField::Dow => i64::from(value.weekday().num_days_from_sunday()),
            ast::DateTimeField::Doy => i64::from(value.ordinal()),
            ast::DateTimeField::Epoch => value
                .and_hms_opt(0, 0, 0)
                .expect("midnight is valid")
                .and_utc()
                .timestamp(),
            _ => {
                return reject_unsupported("date part is not implemented");
            }
        },
        Value::Time(crate::value::PgTime(value)) => match field {
            ast::DateTimeField::Hour => value / 3_600_000_000,
            ast::DateTimeField::Minute => value / 60_000_000 % 60,
            ast::DateTimeField::Second => value / 1_000_000 % 60,
            ast::DateTimeField::Microsecond | ast::DateTimeField::Microseconds => value % 1_000_000,
            ast::DateTimeField::Epoch => value / 1_000_000,
            _ => {
                return reject_unsupported("date part is not implemented");
            }
        },
        Value::Date(crate::value::PgDate::Infinity | crate::value::PgDate::NegInfinity) => {
            return Err(PgError::create(
                SqlState::NumericValueOutOfRange,
                "cannot extract from infinite date",
            ));
        }
        Value::Timestamp(crate::value::PgTimestamp::Finite(value)) => match field {
            ast::DateTimeField::Year => value.year() as i64,
            ast::DateTimeField::Month => i64::from(value.month()),
            ast::DateTimeField::Day => i64::from(value.day()),
            ast::DateTimeField::Hour => i64::from(value.hour()),
            ast::DateTimeField::Minute => i64::from(value.minute()),
            ast::DateTimeField::Second => i64::from(value.second()),
            ast::DateTimeField::Microsecond | ast::DateTimeField::Microseconds => {
                i64::from(value.nanosecond() / 1_000)
            }
            ast::DateTimeField::Epoch => {
                return Ok(convert_epoch_to_numeric(
                    value.and_utc().timestamp(),
                    value.and_utc().timestamp_subsec_micros(),
                ));
            }
            _ => {
                return reject_unsupported("date part is not implemented");
            }
        },
        Value::TimestampTz(crate::value::PgTimestampTz::Finite(value)) => match field {
            ast::DateTimeField::Year => value.year() as i64,
            ast::DateTimeField::Month => i64::from(value.month()),
            ast::DateTimeField::Day => i64::from(value.day()),
            ast::DateTimeField::Hour => i64::from(value.hour()),
            ast::DateTimeField::Minute => i64::from(value.minute()),
            ast::DateTimeField::Second => i64::from(value.second()),
            ast::DateTimeField::Microsecond | ast::DateTimeField::Microseconds => {
                i64::from(value.nanosecond() / 1_000)
            }
            ast::DateTimeField::Epoch => {
                return Ok(convert_epoch_to_numeric(
                    value.timestamp(),
                    value.timestamp_subsec_micros(),
                ));
            }
            _ => {
                return reject_unsupported("date part is not implemented");
            }
        },
        Value::Timestamp(
            crate::value::PgTimestamp::Infinity | crate::value::PgTimestamp::NegInfinity,
        )
        | Value::TimestampTz(
            crate::value::PgTimestampTz::Infinity | crate::value::PgTimestampTz::NegInfinity,
        ) => {
            return Err(PgError::create(
                SqlState::NumericValueOutOfRange,
                "cannot extract from infinite timestamp",
            ));
        }
        _ => {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "extract source must be date or time",
            ));
        }
    };
    Ok(Value::Numeric(value.into()))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn convert_epoch_to_numeric(seconds: i64, subsecond_micros: u32) -> Value {
    Value::Numeric(BigDecimal::from(seconds) + BigDecimal::new(BigInt::from(subsecond_micros), 6))
}
