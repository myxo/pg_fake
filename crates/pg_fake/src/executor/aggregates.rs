use super::*;
use bigdecimal::BigDecimal;
use sqlparser::ast;

#[derive(Clone, Copy)]
enum AggregateKind {
    CountAll,
    Count,
    Sum(BaseType),
    Average(BaseType),
    Minimum,
    Maximum,
    BooleanAnd,
    BooleanOr,
}

struct AggregateCall<'a> {
    kind: AggregateKind,
    argument: Option<&'a ast::Expr>,
    filter: Option<&'a ast::Expr>,
    distinct: bool,
    argument_type: Option<BaseType>,
    result_type: BaseType,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn is_aggregate_function(function: &ast::Function) -> bool {
    function
        .name
        .0
        .first()
        .and_then(ast::ObjectNamePart::as_ident)
        .is_some_and(|name| {
            function.name.0.len() == 1
                && matches!(
                    normalize_identifier(name).as_str(),
                    "count" | "sum" | "avg" | "min" | "max" | "bool_and" | "bool_or"
                )
        })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn infer_aggregate_return_type(
    function: &ast::Function,
    schema: RowScope<'_>,
) -> Result<Option<BaseType>> {
    if !is_aggregate_function(function) {
        return Ok(None);
    }
    parse_aggregate_call(function, schema).map(|call| Some(call.result_type))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_aggregate_call<'a>(
    function: &'a ast::Function,
    schema: RowScope<'_>,
) -> Result<AggregateCall<'a>> {
    let name = normalize_identifier(
        function.name.0[0]
            .as_ident()
            .expect("aggregate name is an identifier"),
    );
    let signature_error = || {
        PgError::create(
            SqlState::UndefinedFunction,
            format!("function {name} does not exist"),
        )
    };
    if function.uses_odbc_syntax
        || !matches!(function.parameters, ast::FunctionArguments::None)
        || function.null_treatment.is_some()
        || function.over.is_some()
        || !function.within_group.is_empty()
    {
        return reject_unsupported("aggregate feature is not implemented");
    }
    let ast::FunctionArguments::List(arguments) = &function.args else {
        return Err(signature_error());
    };
    if !arguments.clauses.is_empty() {
        return reject_unsupported("aggregate argument feature is not implemented");
    }
    if let Some(filter) = &function.filter {
        let data_type = infer_expression_type(filter, schema)?;
        if data_type != BaseType::Bool && !is_null_literal(filter) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "FILTER expression must be type boolean",
            ));
        }
    }
    let distinct = matches!(
        arguments.duplicate_treatment,
        Some(ast::DuplicateTreatment::Distinct)
    );
    if name == "count"
        && (arguments.args.is_empty()
            || matches!(
                arguments.args.as_slice(),
                [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Wildcard)]
            ))
    {
        if distinct {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "DISTINCT requires an aggregate argument",
            ));
        }
        return Ok(AggregateCall {
            kind: AggregateKind::CountAll,
            argument: None,
            filter: function.filter.as_deref(),
            distinct,
            argument_type: None,
            result_type: BaseType::Int8,
        });
    }
    let [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))] =
        arguments.args.as_slice()
    else {
        return Err(signature_error());
    };
    let argument_type = infer_expression_type(argument, schema)?;
    let (kind, result_type) = match name.as_str() {
        "count" => (AggregateKind::Count, BaseType::Int8),
        "sum" => match argument_type {
            BaseType::Int2 | BaseType::Int4 => (AggregateKind::Sum(BaseType::Int8), BaseType::Int8),
            BaseType::Int8 => (AggregateKind::Sum(BaseType::Numeric), BaseType::Numeric),
            BaseType::Float4 | BaseType::Float8 | BaseType::Numeric | BaseType::Interval => {
                (AggregateKind::Sum(argument_type), argument_type)
            }
            _ => return Err(signature_error()),
        },
        "avg" => match argument_type {
            BaseType::Int2 | BaseType::Int4 | BaseType::Int8 | BaseType::Numeric => {
                (AggregateKind::Average(BaseType::Numeric), BaseType::Numeric)
            }
            BaseType::Float4 | BaseType::Float8 => {
                (AggregateKind::Average(BaseType::Float8), BaseType::Float8)
            }
            BaseType::Interval => (
                AggregateKind::Average(BaseType::Interval),
                BaseType::Interval,
            ),
            _ => return Err(signature_error()),
        },
        "min" | "max" => {
            let result_type = match argument_type {
                BaseType::Int2
                | BaseType::Int4
                | BaseType::Int8
                | BaseType::Float4
                | BaseType::Float8
                | BaseType::Numeric
                | BaseType::Text
                | BaseType::Bpchar
                | BaseType::Bytea
                | BaseType::Date
                | BaseType::Time
                | BaseType::Timestamp
                | BaseType::TimestampTz
                | BaseType::Interval => argument_type,
                BaseType::Varchar => BaseType::Text,
                _ => return Err(signature_error()),
            };
            (
                if name == "min" {
                    AggregateKind::Minimum
                } else {
                    AggregateKind::Maximum
                },
                result_type,
            )
        }
        "bool_and" => {
            if argument_type != BaseType::Bool && !is_null_literal(argument) {
                return Err(signature_error());
            }
            (AggregateKind::BooleanAnd, BaseType::Bool)
        }
        "bool_or" => {
            if argument_type != BaseType::Bool && !is_null_literal(argument) {
                return Err(signature_error());
            }
            (AggregateKind::BooleanOr, BaseType::Bool)
        }
        _ => unreachable!("aggregate name was checked"),
    };
    Ok(AggregateCall {
        kind,
        argument: Some(argument),
        filter: function.filter.as_deref(),
        distinct,
        argument_type: Some(argument_type),
        result_type,
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_aggregate_function<F>(
    function: &ast::Function,
    schema: RowScope<'_>,
    rows: &[Vec<Value>],
    mut evaluate_expression: F,
) -> Result<(Value, BaseType)>
where
    F: FnMut(&ast::Expr, &[Value]) -> Result<Value>,
{
    let call = parse_aggregate_call(function, schema)?;
    let mut filtered_rows = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(filter) = call.filter {
            match evaluate_expression(filter, row)? {
                Value::Bool(true) => {}
                Value::Bool(false) | Value::Null => continue,
                _ => unreachable!("aggregate FILTER expression was type-checked"),
            }
        }
        filtered_rows.push(row);
    }
    let Some(argument) = call.argument else {
        return Ok((
            Value::Int8(i64::try_from(filtered_rows.len()).expect("row count must fit in int8")),
            call.result_type,
        ));
    };
    let argument_type = call
        .argument_type
        .expect("aggregate expression has an argument type");
    let mut values = Vec::with_capacity(filtered_rows.len());
    for row in filtered_rows {
        let value = evaluate_expression(argument, row)?;
        if !value.is_null() {
            let duplicate = call.distinct
                && values.iter().try_fold(false, |duplicate, existing| {
                    Ok(duplicate || compare_values(existing, &value)? == Ordering::Equal)
                })?;
            if duplicate {
                continue;
            }
            values.push(value);
        }
    }
    let value = match call.kind {
        AggregateKind::Count => {
            Value::Int8(i64::try_from(values.len()).expect("non-null row count must fit in int8"))
        }
        AggregateKind::Sum(accumulator) | AggregateKind::Average(accumulator) => {
            let mut sum = None;
            for value in values.iter().cloned() {
                let value = coercion::coerce(
                    value,
                    argument_type,
                    PgType::create(accumulator),
                    CastContext::Implicit,
                )?;
                sum = Some(match sum {
                    None => value,
                    Some(current) if accumulator == BaseType::Interval => {
                        evaluate_temporal_arithmetic(&ast::BinaryOperator::Plus, current, value)?
                    }
                    Some(current) => {
                        evaluate_numeric_operator(&ast::BinaryOperator::Plus, current, value)?
                    }
                });
            }
            match (call.kind, sum) {
                (_, None) => Value::Null,
                (AggregateKind::Sum(_), Some(sum)) => sum,
                (AggregateKind::Average(BaseType::Interval), Some(sum)) => {
                    evaluate_temporal_arithmetic(
                        &ast::BinaryOperator::Divide,
                        sum,
                        Value::Float8(values.len() as f64),
                    )?
                }
                (AggregateKind::Average(BaseType::Numeric), Some(sum)) => {
                    evaluate_numeric_operator(
                        &ast::BinaryOperator::Divide,
                        sum,
                        Value::Numeric(BigDecimal::from(
                            i64::try_from(values.len()).expect("row count must fit in int8"),
                        )),
                    )?
                }
                (AggregateKind::Average(BaseType::Float8), Some(sum)) => {
                    let average = evaluate_numeric_operator(
                        &ast::BinaryOperator::Divide,
                        sum,
                        Value::Float8(values.len() as f64),
                    )?;
                    match average {
                        Value::Float8(value) if value == 0.0 => Value::Float8(0.0),
                        average => average,
                    }
                }
                _ => unreachable!("average accumulator type was checked"),
            }
        }
        AggregateKind::Minimum | AggregateKind::Maximum => {
            let mut selected = None;
            for value in values {
                selected = Some(match selected {
                    None => value,
                    Some(current) => {
                        let ordering = compare_values(&value, &current)?;
                        if matches!(call.kind, AggregateKind::Minimum) && ordering == Ordering::Less
                            || matches!(call.kind, AggregateKind::Maximum)
                                && ordering == Ordering::Greater
                        {
                            value
                        } else {
                            current
                        }
                    }
                });
            }
            selected.unwrap_or(Value::Null)
        }
        AggregateKind::BooleanAnd | AggregateKind::BooleanOr => {
            let mut selected = None;
            for value in values {
                let Value::Bool(value) = value else {
                    unreachable!("boolean aggregate argument was type-checked")
                };
                selected = Some(match (call.kind, selected) {
                    (AggregateKind::BooleanAnd, Some(current)) => current && value,
                    (AggregateKind::BooleanOr, Some(current)) => current || value,
                    (_, None) => value,
                    _ => unreachable!("boolean aggregate kind was checked"),
                });
            }
            selected.map(Value::Bool).unwrap_or(Value::Null)
        }
        AggregateKind::CountAll => unreachable!("count all returned before argument evaluation"),
    };
    Ok((value, call.result_type))
}
