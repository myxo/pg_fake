use sqlparser::ast::{self, VisitMut as _};
use std::{cmp::Ordering, collections::BTreeMap};

use crate::{
    coercion::{self, CastContext},
    error::Result,
    executor::{
        DatabaseState, StatementContext,
        aggregates::{AggregateState, parse_aggregate_call, prepare_aggregate_function_input},
        arithmetic::{evaluate_numeric_operator, evaluate_temporal_arithmetic},
        equality::are_rows_not_distinct,
        expressions::{compare_values, infer_window_return_type},
        from::visit_query_source_rows,
        normalize_function_name, resolve_order_ascending,
        scope::{BoundScope, RowScope},
        subqueries::evaluate_query_expression,
    },
    txn::{Snapshot, Xid},
    value::{BaseType, PgType, Value},
};

use super::{
    SelectRow,
    distinct::{DistinctKey, DistinctPlan},
    expressions::{contains_volatile_expression, evaluate_select_expression},
    grouping::{AggregateOwner, GroupedAggregateValues, materialize_aggregate_expression},
    ordering::{OrderKey, RowOrderSpec},
    projection::ProjectionSource,
    select::evaluate_where_clause,
};

struct WindowCollector {
    functions: Vec<(ast::Function, AggregateOwner)>,
    owner: AggregateOwner,
    query_depth: usize,
}

struct WindowMaterializer<'a> {
    functions: &'a [WindowFunction],
    values: &'a [Value],
    owner: AggregateOwner,
    seen: BTreeMap<ast::Function, usize>,
    query_depth: usize,
}

pub(super) struct WindowFunction {
    original: ast::Function,
    function: ast::Function,
    data_type: BaseType,
    owner: AggregateOwner,
}

pub(super) fn collect_window_functions(
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
) -> Vec<(ast::Function, AggregateOwner)> {
    let mut collector = WindowCollector {
        functions: Vec::new(),
        owner: AggregateOwner::Projection(0),
        query_depth: 0,
    };
    let mut visit = |owner, expression: &ast::Expr| {
        collector.owner = owner;
        let mut expression = expression.clone();
        let _ = expression.visit(&mut collector);
    };
    for (index, projection) in projections.iter().enumerate() {
        if let ProjectionSource::Expression(expression) = projection {
            visit(AggregateOwner::Projection(index), expression);
        }
    }
    for (index, order) in order_specs.iter().enumerate() {
        if let OrderKey::Expression(expression) = order.key {
            visit(AggregateOwner::Order(index), expression);
        }
    }
    if let DistinctPlan::On { expressions, .. } = distinct {
        for (index, expression) in expressions.iter().enumerate() {
            visit(AggregateOwner::Distinct(index), expression);
        }
    }
    collector.functions
}

pub(super) fn resolve_select_windows(select: &mut ast::Select) -> Result<()> {
    let mut windows = BTreeMap::new();
    for ast::NamedWindowDefinition(name, definition) in &select.named_window {
        let name = crate::executor::normalize_identifier(name);
        if windows.contains_key(&name) {
            return Err(crate::error::PgError::create(
                crate::error::SqlState::WindowingError,
                format!("window {name} is already defined"),
            ));
        }
        let window = match definition {
            ast::NamedWindowExpr::NamedWindow(parent) => windows
                .get(&crate::executor::normalize_identifier(parent))
                .cloned()
                .ok_or_else(|| {
                    crate::error::PgError::create(
                        crate::error::SqlState::WindowingError,
                        format!("window {} does not exist", parent.value),
                    )
                })?,
            ast::NamedWindowExpr::WindowSpec(window) => {
                resolve_window_spec(window.clone(), &windows)?
            }
        };
        windows.insert(name, window);
    }
    struct Resolver<'a> {
        windows: &'a BTreeMap<String, ast::WindowSpec>,
        error: Option<crate::error::PgError>,
        query_depth: usize,
    }
    impl ast::VisitorMut for Resolver<'_> {
        type Break = ();

        fn pre_visit_query(
            &mut self,
            _query: &mut ast::Query,
        ) -> std::ops::ControlFlow<Self::Break> {
            self.query_depth += 1;
            std::ops::ControlFlow::Continue(())
        }

        fn post_visit_query(
            &mut self,
            _query: &mut ast::Query,
        ) -> std::ops::ControlFlow<Self::Break> {
            self.query_depth -= 1;
            std::ops::ControlFlow::Continue(())
        }

        fn pre_visit_expr(
            &mut self,
            expression: &mut ast::Expr,
        ) -> std::ops::ControlFlow<Self::Break> {
            if self.query_depth != 0 {
                return std::ops::ControlFlow::Continue(());
            }
            let ast::Expr::Function(function) = expression else {
                return std::ops::ControlFlow::Continue(());
            };
            let Some(over) = &function.over else {
                return std::ops::ControlFlow::Continue(());
            };
            let resolved = match over {
                ast::WindowType::NamedWindow(name) => self
                    .windows
                    .get(&crate::executor::normalize_identifier(name))
                    .cloned()
                    .ok_or_else(|| {
                        crate::error::PgError::create(
                            crate::error::SqlState::WindowingError,
                            format!("window {} does not exist", name.value),
                        )
                    }),
                ast::WindowType::WindowSpec(window) => {
                    resolve_window_spec(window.clone(), self.windows)
                }
            };
            match resolved {
                Ok(window) => function.over = Some(ast::WindowType::WindowSpec(window)),
                Err(error) => {
                    self.error = Some(error);
                    return std::ops::ControlFlow::Break(());
                }
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut resolver = Resolver {
        windows: &windows,
        error: None,
        query_depth: 0,
    };
    let _ = select.visit(&mut resolver);
    resolver.error.map_or(Ok(()), Err)
}

pub(crate) fn resolve_statement_windows(statement: &mut ast::Statement) -> Result<()> {
    struct Resolver {
        error: Option<crate::error::PgError>,
    }
    impl ast::VisitorMut for Resolver {
        type Break = ();

        fn pre_visit_select(
            &mut self,
            select: &mut ast::Select,
        ) -> std::ops::ControlFlow<Self::Break> {
            if let Err(error) = resolve_select_windows(select) {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut resolver = Resolver { error: None };
    let _ = statement.visit(&mut resolver);
    resolver.error.map_or(Ok(()), Err)
}

fn resolve_window_spec(
    mut window: ast::WindowSpec,
    windows: &BTreeMap<String, ast::WindowSpec>,
) -> Result<ast::WindowSpec> {
    let Some(parent_name) = window.window_name.take() else {
        return Ok(window);
    };
    let parent = windows
        .get(&crate::executor::normalize_identifier(&parent_name))
        .ok_or_else(|| {
            crate::error::PgError::create(
                crate::error::SqlState::WindowingError,
                format!("window {} does not exist", parent_name.value),
            )
        })?;
    if !window.partition_by.is_empty() && !parent.partition_by.is_empty() {
        return Err(crate::error::PgError::create(
            crate::error::SqlState::WindowingError,
            "cannot override PARTITION BY clause of window",
        ));
    }
    if !window.order_by.is_empty() && !parent.order_by.is_empty() {
        return Err(crate::error::PgError::create(
            crate::error::SqlState::WindowingError,
            "cannot override ORDER BY clause of window",
        ));
    }
    if parent.window_frame.is_some() {
        return Err(crate::error::PgError::create(
            crate::error::SqlState::WindowingError,
            "cannot copy window because it has a frame clause",
        ));
    }
    if window.partition_by.is_empty() {
        window.partition_by = parent.partition_by.clone();
    }
    if window.order_by.is_empty() {
        window.order_by = parent.order_by.clone();
    }
    Ok(window)
}

pub(super) fn resolve_window_functions(
    functions: Vec<(ast::Function, AggregateOwner)>,
    scope: &BoundScope,
) -> Result<Vec<WindowFunction>> {
    functions
        .into_iter()
        .map(|(function, owner)| {
            let data_type = infer_window_return_type(&function, RowScope::Bound(scope))?
                .expect("collected function is a window function");
            Ok(WindowFunction {
                original: function.clone(),
                function,
                data_type,
                owner,
            })
        })
        .collect()
}

pub(super) fn materialize_window_expression(
    expression: &ast::Expr,
    functions: &[WindowFunction],
    values: &[Value],
    owner: AggregateOwner,
) -> ast::Expr {
    assert_eq!(functions.len(), values.len());
    let mut expression = expression.clone();
    let _ = expression.visit(&mut WindowMaterializer {
        functions,
        values,
        owner,
        seen: BTreeMap::new(),
        query_depth: 0,
    });
    expression
}

fn compare_window_order_keys(
    left: &[Value],
    right: &[Value],
    orders: &[ast::OrderByExpr],
) -> Ordering {
    orders
        .iter()
        .zip(left.iter().zip(right))
        .find_map(|(order, (left, right))| {
            let ascending = resolve_order_ascending(&order.options)
                .expect("window ORDER BY variant was validated");
            let nulls_first = order.options.nulls_first.unwrap_or(!ascending);
            let ordering = match (left, right) {
                (Value::Null, Value::Null) => Ordering::Equal,
                (Value::Null, _) => {
                    if nulls_first {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (_, Value::Null) => {
                    if nulls_first {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                _ => {
                    let ordering =
                        compare_values(left, right).expect("window ORDER BY type was validated");
                    if ascending {
                        ordering
                    } else {
                        ordering.reverse()
                    }
                }
            };
            (ordering != Ordering::Equal).then_some(ordering)
        })
        .unwrap_or(Ordering::Equal)
}

fn evaluate_window_expression(
    state: &DatabaseState,
    function: &WindowFunction,
    expression: &ast::Expr,
    scope: &BoundScope,
    row: &[Value],
    aggregate_values: Option<&GroupedAggregateValues>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Value> {
    let expression = aggregate_values.map_or_else(
        || Ok(expression.clone()),
        |values| materialize_aggregate_expression(state, expression, scope, values, function.owner),
    )?;
    evaluate_query_expression(state, &expression, scope, row, xid, snapshot, context)
}

fn evaluate_window_value_expression(
    state: &DatabaseState,
    function: &WindowFunction,
    expression: &ast::Expr,
    scope: &BoundScope,
    row: &[Value],
    aggregate_values: Option<&GroupedAggregateValues>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Value> {
    let expression = aggregate_values.map_or_else(
        || Ok(expression.clone()),
        |values| materialize_aggregate_expression(state, expression, scope, values, function.owner),
    )?;
    let value = evaluate_query_expression(state, &expression, scope, row, xid, snapshot, context)?;
    if value.is_null() {
        return Ok(value);
    }
    if let Some(text) = crate::executor::extract_unknown_string_literal(&expression) {
        return coercion::coerce_unknown(
            text,
            PgType::create(function.data_type),
            CastContext::Implicit,
            &context.get_literal_timezone(),
        );
    }
    coercion::coerce(
        value,
        crate::executor::infer_expression_type(&expression, RowScope::Bound(scope))?,
        PgType::create(function.data_type),
        CastContext::Implicit,
        &context.get_timezone(),
    )
}

fn evaluate_window_offset(
    state: &DatabaseState,
    function: &WindowFunction,
    expression: &ast::Expr,
    scope: &BoundScope,
    row: &[Value],
    aggregate_values: Option<&GroupedAggregateValues>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Option<i64>> {
    let value = evaluate_window_expression(
        state,
        function,
        expression,
        scope,
        row,
        aggregate_values,
        xid,
        snapshot,
        context,
    )?;
    let value = match value {
        Value::Null => return Ok(None),
        Value::Text(value) => coercion::coerce_unknown(
            &value,
            PgType::create(BaseType::Int4),
            CastContext::Implicit,
            &context.get_timezone(),
        )?,
        value => coercion::coerce(
            value.clone(),
            value
                .get_base_type()
                .expect("non-null offset has a base type"),
            PgType::create(BaseType::Int4),
            CastContext::Implicit,
            &context.get_timezone(),
        )?,
    };
    let Value::Int4(value) = value else {
        unreachable!("window offset was coerced to int4")
    };
    Ok(Some(i64::from(value)))
}

fn is_window_aggregate(name: &str) -> bool {
    matches!(
        name,
        "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
            | "bool_and"
            | "bool_or"
            | "string_agg"
            | "array_agg"
    )
}

fn is_negative_frame_offset(value: &Value) -> bool {
    match value {
        Value::Int2(value) => *value < 0,
        Value::Int4(value) => *value < 0,
        Value::Int8(value) => *value < 0,
        Value::Float4(value) => *value < 0.0 || value.is_nan(),
        Value::Float8(value) => *value < 0.0 || value.is_nan(),
        Value::Numeric(value) => value < &bigdecimal::BigDecimal::from(0),
        _ => false,
    }
}

fn interval_comparison_value(value: &crate::value::PgInterval) -> i128 {
    i128::from(value.months)
        * i128::from(crate::value::DAYS_PER_MONTH)
        * i128::from(crate::value::MICROSECONDS_PER_DAY)
        + i128::from(value.days) * i128::from(crate::value::MICROSECONDS_PER_DAY)
        + i128::from(value.micros)
}

fn invalid_frame_offset() -> crate::error::PgError {
    crate::error::PgError::create(
        crate::error::SqlState::InvalidPrecedingOrFollowingSize,
        "invalid preceding or following size in window function",
    )
}

fn evaluate_frame_offset(
    state: &DatabaseState,
    function: &WindowFunction,
    expression: &ast::Expr,
    scope: &BoundScope,
    row: &[Value],
    aggregate_values: Option<&GroupedAggregateValues>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    validate_sign: bool,
) -> Result<Value> {
    let mut value = evaluate_window_expression(
        state,
        function,
        expression,
        scope,
        row,
        aggregate_values,
        xid,
        snapshot,
        context,
    )?;
    if value.is_null() {
        return Err(crate::error::PgError::create(
            crate::error::SqlState::NullValueNotAllowed,
            "frame offset must not be null",
        ));
    }
    if validate_sign {
        value = Value::Int8(coerce_frame_count(value, context)?);
        if is_negative_frame_offset(&value) {
            return Err(invalid_frame_offset());
        }
    }
    Ok(value)
}

fn coerce_frame_count(value: Value, context: &StatementContext) -> Result<i64> {
    let value = match value {
        Value::Text(value) => coercion::coerce_unknown(
            &value,
            PgType::create(BaseType::Int8),
            CastContext::Implicit,
            &context.get_timezone(),
        )?,
        value => coercion::coerce(
            value.clone(),
            value.get_base_type().expect("frame offset has a type"),
            PgType::create(BaseType::Int8),
            CastContext::Implicit,
            &context.get_timezone(),
        )?,
    };
    let Value::Int8(value) = value else {
        unreachable!("frame count was coerced to int8")
    };
    Ok(value)
}

fn calculate_range_threshold(
    current: &Value,
    offset: Value,
    preceding: bool,
    descending: bool,
    context: &StatementContext,
) -> Result<Value> {
    if current.is_null() {
        return Ok(Value::Null);
    }
    let offset = if matches!(
        current,
        Value::Date(_)
            | Value::Time(_)
            | Value::Timestamp(_)
            | Value::TimestampTz(_)
            | Value::Interval(_)
    ) {
        match offset {
            Value::Text(value) => coercion::coerce_unknown(
                &value,
                PgType::create(BaseType::Interval),
                CastContext::Implicit,
                &context.get_timezone(),
            )?,
            offset => offset,
        }
    } else {
        offset
    };
    if matches!(
        current,
        Value::Date(_) | Value::Timestamp(_) | Value::TimestampTz(_)
    ) && let Value::Interval(offset) = &offset
        && interval_comparison_value(offset) < 0
    {
        return Err(invalid_frame_offset());
    }
    let operator = if preceding != descending {
        ast::BinaryOperator::Minus
    } else {
        ast::BinaryOperator::Plus
    };
    match current {
        Value::Int2(_) | Value::Int4(_) | Value::Int8(_) => {
            let current = match current {
                Value::Int2(value) => bigdecimal::BigDecimal::from(*value),
                Value::Int4(value) => bigdecimal::BigDecimal::from(*value),
                Value::Int8(value) => bigdecimal::BigDecimal::from(*value),
                _ => unreachable!("integer range key was matched"),
            };
            let offset = match offset {
                Value::Text(value) => coercion::coerce_unknown(
                    &value,
                    PgType::create(BaseType::Numeric),
                    CastContext::Implicit,
                    &context.get_timezone(),
                )?,
                offset => coercion::coerce(
                    offset.clone(),
                    offset.get_base_type().expect("range offset has a type"),
                    PgType::create(BaseType::Numeric),
                    CastContext::Implicit,
                    &context.get_timezone(),
                )?,
            };
            let Value::Numeric(offset) = offset else {
                unreachable!("integer range offset was coerced to numeric")
            };
            if offset < 0 {
                return Err(invalid_frame_offset());
            }
            Ok(Value::Numeric(
                if matches!(operator, ast::BinaryOperator::Minus) {
                    current - offset
                } else {
                    current + offset
                },
            ))
        }
        Value::Float4(_) | Value::Float8(_) | Value::Numeric(_) => {
            let data_type = current.get_base_type().expect("range key has a type");
            let offset = match offset {
                Value::Text(value) => coercion::coerce_unknown(
                    &value,
                    PgType::create(data_type),
                    CastContext::Implicit,
                    &context.get_timezone(),
                )?,
                offset => coercion::coerce(
                    offset.clone(),
                    offset.get_base_type().expect("range offset has a type"),
                    PgType::create(data_type),
                    CastContext::Implicit,
                    &context.get_timezone(),
                )?,
            };
            if is_negative_frame_offset(&offset) {
                return Err(invalid_frame_offset());
            }
            match offset {
                Value::Float4(value) if value.is_infinite() => {
                    return Ok(Value::Float4(
                        if matches!(operator, ast::BinaryOperator::Minus) {
                            f32::NEG_INFINITY
                        } else {
                            f32::INFINITY
                        },
                    ));
                }
                Value::Float8(value) if value.is_infinite() => {
                    return Ok(Value::Float8(
                        if matches!(operator, ast::BinaryOperator::Minus) {
                            f64::NEG_INFINITY
                        } else {
                            f64::INFINITY
                        },
                    ));
                }
                _ => {}
            }
            evaluate_numeric_operator(&operator, current.clone(), offset)
        }
        Value::Date(crate::value::PgDate::NegInfinity)
        | Value::Date(crate::value::PgDate::Infinity)
        | Value::Timestamp(crate::value::PgTimestamp::NegInfinity)
        | Value::Timestamp(crate::value::PgTimestamp::Infinity)
        | Value::TimestampTz(crate::value::PgTimestampTz::NegInfinity)
        | Value::TimestampTz(crate::value::PgTimestampTz::Infinity) => Ok(current.clone()),
        Value::Time(current) => {
            let Value::Interval(offset) = offset else {
                return Err(crate::error::PgError::create(
                    crate::error::SqlState::DatatypeMismatch,
                    "RANGE offset has incompatible type",
                ));
            };
            if offset.micros < 0 {
                return Err(invalid_frame_offset());
            }
            let offset = i128::from(offset.micros);
            let value = i128::from(current.0)
                + if matches!(operator, ast::BinaryOperator::Minus) {
                    -offset
                } else {
                    offset
                };
            Ok(Value::Numeric(bigdecimal::BigDecimal::from(value)))
        }
        Value::Interval(current) => {
            let Value::Interval(offset) = offset else {
                return Err(crate::error::PgError::create(
                    crate::error::SqlState::DatatypeMismatch,
                    "RANGE offset has incompatible type",
                ));
            };
            let current = interval_comparison_value(current);
            let offset = interval_comparison_value(&offset);
            if offset < 0 {
                return Err(invalid_frame_offset());
            }
            Ok(Value::Numeric(bigdecimal::BigDecimal::from(
                current
                    + if matches!(operator, ast::BinaryOperator::Minus) {
                        -offset
                    } else {
                        offset
                    },
            )))
        }
        Value::Date(_) | Value::Timestamp(_) | Value::TimestampTz(_) => {
            evaluate_temporal_arithmetic(&operator, current.clone(), offset)
        }
        _ => Err(crate::error::PgError::create(
            crate::error::SqlState::FeatureNotSupported,
            "RANGE offset type is not supported",
        )),
    }
}

fn compare_range_key(candidate: &Value, threshold: &Value, order: &ast::OrderByExpr) -> Ordering {
    let ascending =
        resolve_order_ascending(&order.options).expect("window ORDER BY variant was validated");
    let nulls_first = order.options.nulls_first.unwrap_or(!ascending);
    let ordering = match (candidate, threshold) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (_, Value::Null) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Value::Int2(candidate), Value::Numeric(threshold)) => {
            bigdecimal::BigDecimal::from(*candidate).cmp(threshold)
        }
        (Value::Int4(candidate), Value::Numeric(threshold)) => {
            bigdecimal::BigDecimal::from(*candidate).cmp(threshold)
        }
        (Value::Int8(candidate), Value::Numeric(threshold)) => {
            bigdecimal::BigDecimal::from(*candidate).cmp(threshold)
        }
        (Value::Time(candidate), Value::Numeric(threshold)) => {
            bigdecimal::BigDecimal::from(candidate.0).cmp(threshold)
        }
        (Value::Interval(candidate), Value::Numeric(threshold)) => {
            let candidate = i128::from(candidate.months)
                * i128::from(crate::value::DAYS_PER_MONTH)
                * i128::from(crate::value::MICROSECONDS_PER_DAY)
                + i128::from(candidate.days) * i128::from(crate::value::MICROSECONDS_PER_DAY)
                + i128::from(candidate.micros);
            bigdecimal::BigDecimal::from(candidate).cmp(threshold)
        }
        (Value::Date(candidate), Value::Timestamp(threshold)) => {
            let candidate = match candidate {
                crate::value::PgDate::NegInfinity => crate::value::PgTimestamp::NegInfinity,
                crate::value::PgDate::Finite(candidate) => crate::value::PgTimestamp::Finite(
                    candidate.and_hms_opt(0, 0, 0).expect("midnight is valid"),
                ),
                crate::value::PgDate::Infinity => crate::value::PgTimestamp::Infinity,
            };
            candidate.cmp(threshold)
        }
        _ => compare_values(candidate, threshold).expect("RANGE types were validated"),
    };
    if ascending {
        ordering
    } else {
        ordering.reverse()
    }
}

fn calculate_frame_positions(
    window: &ast::WindowSpec,
    indexes: &[usize],
    keys: &[Vec<Value>],
    peer_ranges: &[(usize, usize)],
    peer_index: usize,
    position: usize,
    frame_offsets: &[Option<Value>; 2],
    context: &StatementContext,
) -> Result<Vec<usize>> {
    let Some(frame) = &window.window_frame else {
        let (start, end) = if window.order_by.is_empty() {
            (0, indexes.len())
        } else {
            (0, peer_ranges[peer_index].1)
        };
        return Ok(indexes[start..end].to_vec());
    };
    let current_row = indexes[position];
    let resolve_bound = |bound: &ast::WindowFrameBound, start: bool| -> Result<i128> {
        match bound {
            ast::WindowFrameBound::Preceding(None) => Ok(0),
            ast::WindowFrameBound::Following(None) => Ok(indexes.len() as i128 - 1),
            ast::WindowFrameBound::CurrentRow => Ok(match frame.units {
                ast::WindowFrameUnits::Rows => position as i128,
                ast::WindowFrameUnits::Range | ast::WindowFrameUnits::Groups => {
                    let (peer_start, peer_end) = peer_ranges[peer_index];
                    if start {
                        peer_start as i128
                    } else {
                        peer_end as i128 - 1
                    }
                }
            }),
            ast::WindowFrameBound::Preceding(Some(offset))
            | ast::WindowFrameBound::Following(Some(offset)) => {
                let preceding = matches!(bound, ast::WindowFrameBound::Preceding(_));
                let _ = offset;
                let offset = frame_offsets[usize::from(!start)]
                    .clone()
                    .expect("frame offset was evaluated");
                match frame.units {
                    ast::WindowFrameUnits::Rows => {
                        let offset = coerce_frame_count(offset, context)?;
                        if offset < 0 {
                            return Err(crate::error::PgError::create(
                                crate::error::SqlState::InvalidPrecedingOrFollowingSize,
                                "invalid preceding or following size in window function",
                            ));
                        }
                        Ok(position as i128
                            + if preceding {
                                -i128::from(offset)
                            } else {
                                i128::from(offset)
                            })
                    }
                    ast::WindowFrameUnits::Groups => {
                        let offset = coerce_frame_count(offset, context)?;
                        if offset < 0 {
                            return Err(crate::error::PgError::create(
                                crate::error::SqlState::InvalidPrecedingOrFollowingSize,
                                "invalid preceding or following size in window function",
                            ));
                        }
                        let target = peer_index as i128
                            + if preceding {
                                -i128::from(offset)
                            } else {
                                i128::from(offset)
                            };
                        if target < 0 {
                            return Ok(-1);
                        }
                        let Ok(target) = usize::try_from(target) else {
                            return Ok(indexes.len() as i128);
                        };
                        let Some((peer_start, peer_end)) = peer_ranges.get(target).copied() else {
                            return Ok(indexes.len() as i128);
                        };
                        Ok(if start {
                            peer_start as i128
                        } else {
                            peer_end as i128 - 1
                        })
                    }
                    ast::WindowFrameUnits::Range => {
                        let current = &keys[current_row][0];
                        if current.is_null() {
                            let (peer_start, peer_end) = peer_ranges[peer_index];
                            return Ok(if start {
                                peer_start as i128
                            } else {
                                peer_end as i128 - 1
                            });
                        }
                        let descending = matches!(
                            window.order_by[0].options.sort,
                            Some(ast::OrderBySort::Desc)
                        );
                        let threshold = calculate_range_threshold(
                            current, offset, preceding, descending, context,
                        )?;
                        let threshold = [threshold];
                        if start {
                            Ok(indexes
                                .iter()
                                .position(|index| {
                                    compare_range_key(
                                        &keys[*index][0],
                                        &threshold[0],
                                        &window.order_by[0],
                                    ) != Ordering::Less
                                })
                                .map_or(indexes.len() as i128, |index| index as i128))
                        } else {
                            Ok(indexes
                                .iter()
                                .rposition(|index| {
                                    compare_range_key(
                                        &keys[*index][0],
                                        &threshold[0],
                                        &window.order_by[0],
                                    ) != Ordering::Greater
                                })
                                .map_or(-1, |index| index as i128))
                        }
                    }
                }
            }
        }
    };
    let start = resolve_bound(&frame.start_bound, true)?.max(0);
    let end = resolve_bound(
        frame
            .end_bound
            .as_ref()
            .unwrap_or(&ast::WindowFrameBound::CurrentRow),
        false,
    )?
    .min(indexes.len() as i128 - 1);
    if start > end || start >= indexes.len() as i128 || end < 0 {
        return Ok(Vec::new());
    }
    Ok(indexes[start as usize..=end as usize].to_vec())
}

pub(super) fn calculate_window_values(
    state: &DatabaseState,
    functions: &[WindowFunction],
    scope: &BoundScope,
    rows: &[Vec<Value>],
    aggregate_values: Option<&[GroupedAggregateValues]>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Vec<Vec<Value>>> {
    let mut values = vec![vec![Value::Null; functions.len()]; rows.len()];
    for (function_index, window_function) in functions.iter().enumerate() {
        let function = &window_function.function;
        let name = normalize_function_name(&function.name)?;
        let ast::WindowType::WindowSpec(window) = function
            .over
            .as_ref()
            .expect("collected window function has OVER")
        else {
            unreachable!("named windows were rejected")
        };
        let partitions = rows
            .iter()
            .enumerate()
            .map(|(row_index, row)| {
                window
                    .partition_by
                    .iter()
                    .map(|expression| {
                        evaluate_window_expression(
                            state,
                            window_function,
                            expression,
                            scope,
                            row,
                            aggregate_values.map(|values| &values[row_index]),
                            xid,
                            snapshot,
                            context,
                        )
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        let keys = rows
            .iter()
            .enumerate()
            .map(|(row_index, row)| {
                window
                    .order_by
                    .iter()
                    .map(|order| {
                        evaluate_window_expression(
                            state,
                            window_function,
                            &order.expr,
                            scope,
                            row,
                            aggregate_values.map(|values| &values[row_index]),
                            xid,
                            snapshot,
                            context,
                        )
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        let mut groups = Vec::<Vec<usize>>::new();
        for index in 0..rows.len() {
            if let Some(group) = groups.iter_mut().find(|group| {
                are_rows_not_distinct(&partitions[index], &partitions[group[0]])
                    .expect("partition values have validated equality")
            }) {
                group.push(index);
            } else {
                groups.push(vec![index]);
            }
        }
        let aggregate_function = if is_window_aggregate(&name) {
            let mut aggregate = function.clone();
            aggregate.over = None;
            Some(aggregate)
        } else {
            None
        };
        let aggregate_call = aggregate_function
            .as_ref()
            .map(|aggregate| parse_aggregate_call(aggregate, RowScope::Bound(scope)))
            .transpose()?;
        let mut frame_offsets = [None, None];
        if let Some(frame) = &window.window_frame {
            for (index, bound) in [
                &frame.start_bound,
                frame
                    .end_bound
                    .as_ref()
                    .unwrap_or(&ast::WindowFrameBound::CurrentRow),
            ]
            .into_iter()
            .enumerate()
            {
                if let ast::WindowFrameBound::Preceding(Some(offset))
                | ast::WindowFrameBound::Following(Some(offset)) = bound
                {
                    frame_offsets[index] = Some(evaluate_frame_offset(
                        state,
                        window_function,
                        offset,
                        scope,
                        &[],
                        None,
                        xid,
                        snapshot,
                        context,
                        !matches!(frame.units, ast::WindowFrameUnits::Range),
                    )?);
                }
            }
        }
        for indexes in &mut groups {
            indexes.sort_by(|left, right| {
                compare_window_order_keys(&keys[*left], &keys[*right], &window.order_by)
            });
            let mut peer_ranges = Vec::new();
            let mut peer_by_position = vec![0; indexes.len()];
            let mut peer_start = 0;
            while peer_start < indexes.len() {
                let mut peer_end = peer_start + 1;
                while peer_end < indexes.len()
                    && compare_window_order_keys(
                        &keys[indexes[peer_start]],
                        &keys[indexes[peer_end]],
                        &window.order_by,
                    ) == Ordering::Equal
                {
                    peer_end += 1;
                }
                let peer_index = peer_ranges.len();
                peer_by_position[peer_start..peer_end].fill(peer_index);
                peer_ranges.push((peer_start, peer_end));
                peer_start = peer_end;
            }
            let bucket_count = if name == "ntile" {
                let ast::FunctionArguments::List(arguments) = &function.args else {
                    unreachable!("ntile arguments were validated")
                };
                let ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument)) =
                    &arguments.args[0]
                else {
                    unreachable!("ntile argument was validated")
                };
                let value = evaluate_window_expression(
                    state,
                    window_function,
                    argument,
                    scope,
                    &rows[indexes[0]],
                    aggregate_values.map(|values| &values[indexes[0]]),
                    xid,
                    snapshot,
                    context,
                )?;
                match value {
                    Value::Null => {
                        return Err(crate::error::PgError::create(
                            crate::error::SqlState::NullValueNotAllowed,
                            "argument of ntile must not be null",
                        ));
                    }
                    Value::Text(value) => match coercion::coerce_unknown(
                        &value,
                        PgType::create(BaseType::Int4),
                        CastContext::Implicit,
                        &context.get_timezone(),
                    )? {
                        Value::Int4(value) => i64::from(value),
                        _ => unreachable!("ntile literal was coerced to int4"),
                    },
                    Value::Int2(value) => i64::from(value),
                    Value::Int4(value) => i64::from(value),
                    Value::Int8(value) => value,
                    _ => unreachable!("ntile argument was type-checked"),
                }
            } else {
                0
            };
            if name == "ntile" && bucket_count <= 0 {
                return Err(crate::error::PgError::create(
                    crate::error::SqlState::InvalidParameterValue,
                    "argument of ntile must be greater than zero",
                ));
            }
            for position in 0..indexes.len() {
                let row_index = indexes[position];
                let peer_index = peer_by_position[position];
                let (peer_start, peer_end) = peer_ranges[peer_index];
                let frame = calculate_frame_positions(
                    window,
                    indexes,
                    &keys,
                    &peer_ranges,
                    peer_index,
                    position,
                    &frame_offsets,
                    context,
                )?;
                values[row_index][function_index] = match name.as_str() {
                    "row_number" => Value::Int8(
                        i64::try_from(position + 1).expect("row number must fit in int8"),
                    ),
                    "rank" => {
                        Value::Int8(i64::try_from(peer_start + 1).expect("rank must fit in int8"))
                    }
                    "dense_rank" => Value::Int8(
                        i64::try_from(peer_index + 1).expect("dense rank must fit in int8"),
                    ),
                    "percent_rank" => Value::Float8(if indexes.len() == 1 {
                        0.0
                    } else {
                        peer_start as f64 / (indexes.len() - 1) as f64
                    }),
                    "cume_dist" => Value::Float8(peer_end as f64 / indexes.len() as f64),
                    "ntile" => {
                        let row_count = i64::try_from(indexes.len())
                            .expect("partition length must fit in int8");
                        let position =
                            i64::try_from(position).expect("window position must fit in int8");
                        let large_buckets = row_count % bucket_count;
                        let large_size = row_count / bucket_count + 1;
                        let bucket = if position < large_buckets * large_size {
                            position / large_size + 1
                        } else {
                            (position - large_buckets * large_size) / (row_count / bucket_count)
                                + large_buckets
                                + 1
                        };
                        Value::Int4(i32::try_from(bucket).expect("ntile result must fit in int4"))
                    }
                    "lag" | "lead" => {
                        let ast::FunctionArguments::List(arguments) = &function.args else {
                            unreachable!("lag and lead arguments were validated")
                        };
                        let ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(value)) =
                            &arguments.args[0]
                        else {
                            unreachable!("lag and lead value argument was validated")
                        };
                        let offset = if let Some(argument) = arguments.args.get(1) {
                            let ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument)) =
                                argument
                            else {
                                unreachable!("lag and lead offset argument was validated")
                            };
                            evaluate_window_offset(
                                state,
                                window_function,
                                argument,
                                scope,
                                &rows[row_index],
                                aggregate_values.map(|values| &values[row_index]),
                                xid,
                                snapshot,
                                context,
                            )?
                        } else {
                            Some(1)
                        };
                        match offset {
                            None => Value::Null,
                            Some(offset) => {
                                let target = if name == "lag" {
                                    i64::try_from(position)
                                        .expect("window position must fit in int8")
                                        - offset
                                } else {
                                    i64::try_from(position)
                                        .expect("window position must fit in int8")
                                        + offset
                                };
                                if let Ok(target) = usize::try_from(target)
                                    && let Some(&target) = indexes.get(target)
                                {
                                    evaluate_window_value_expression(
                                        state,
                                        window_function,
                                        value,
                                        scope,
                                        &rows[target],
                                        aggregate_values.map(|values| &values[target]),
                                        xid,
                                        snapshot,
                                        context,
                                    )?
                                } else if let Some(argument) = arguments.args.get(2) {
                                    let ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(
                                        default,
                                    )) = argument
                                    else {
                                        unreachable!("lag and lead default argument was validated")
                                    };
                                    evaluate_window_value_expression(
                                        state,
                                        window_function,
                                        default,
                                        scope,
                                        &rows[row_index],
                                        aggregate_values.map(|values| &values[row_index]),
                                        xid,
                                        snapshot,
                                        context,
                                    )?
                                } else {
                                    Value::Null
                                }
                            }
                        }
                    }
                    "first_value" | "last_value" | "nth_value" => {
                        let ast::FunctionArguments::List(arguments) = &function.args else {
                            unreachable!("value function arguments were validated")
                        };
                        let ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(value)) =
                            &arguments.args[0]
                        else {
                            unreachable!("value function argument was validated")
                        };
                        let target = if name == "first_value" {
                            frame.first().copied()
                        } else if name == "last_value" {
                            frame.last().copied()
                        } else {
                            let ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(offset)) =
                                &arguments.args[1]
                            else {
                                unreachable!("nth_value offset argument was validated")
                            };
                            match evaluate_window_offset(
                                state,
                                window_function,
                                offset,
                                scope,
                                &rows[row_index],
                                aggregate_values.map(|values| &values[row_index]),
                                xid,
                                snapshot,
                                context,
                            )? {
                                None => None,
                                Some(offset) if offset <= 0 => {
                                    return Err(crate::error::PgError::create(
                                        crate::error::SqlState::InvalidArgumentForNthValue,
                                        "argument of nth_value must be greater than zero",
                                    ));
                                }
                                Some(offset) => usize::try_from(offset - 1)
                                    .ok()
                                    .and_then(|offset| frame.get(offset).copied()),
                            }
                        };
                        if let Some(target) = target {
                            evaluate_window_value_expression(
                                state,
                                window_function,
                                value,
                                scope,
                                &rows[target],
                                aggregate_values.map(|values| &values[target]),
                                xid,
                                snapshot,
                                context,
                            )?
                        } else {
                            Value::Null
                        }
                    }
                    name if is_window_aggregate(name) => {
                        let call = aggregate_call
                            .as_ref()
                            .expect("window aggregate was parsed");
                        let mut aggregate = AggregateState::create(&call.descriptor);
                        for target in frame {
                            let input = prepare_aggregate_function_input(call, |expression| {
                                evaluate_window_expression(
                                    state,
                                    window_function,
                                    expression,
                                    scope,
                                    &rows[target],
                                    aggregate_values.map(|values| &values[target]),
                                    xid,
                                    snapshot,
                                    context,
                                )
                            })?;
                            aggregate.add_input(&call.descriptor, input);
                        }
                        aggregate.finish(&call.descriptor)?.0
                    }
                    _ => unreachable!("window function name was validated"),
                };
            }
        }
    }
    Ok(values)
}

pub(super) fn execute_windowed_select_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    functions: &[WindowFunction],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Vec<SelectRow>> {
    let mut source_rows = Vec::new();
    visit_query_source_rows(
        state,
        select,
        scope,
        xid,
        snapshot,
        context,
        select.selection.as_ref(),
        &mut |row, _origins| {
            if evaluate_where_clause(
                state,
                select.selection.as_ref(),
                scope,
                row,
                xid,
                snapshot,
                context,
            )? {
                source_rows.push(row.to_vec());
            }
            Ok(())
        },
    )?;
    let window_values = calculate_window_values(
        state,
        functions,
        scope,
        &source_rows,
        None,
        xid,
        snapshot,
        context,
    )?;
    source_rows
        .iter()
        .zip(&window_values)
        .map(|(row, window_values)| {
            let values = projections
                .iter()
                .enumerate()
                .map(|(index, projection)| match projection {
                    ProjectionSource::Column(index) => Ok(row[*index].clone()),
                    ProjectionSource::Merged(slots, data_type, _) => {
                        let value = slots
                            .iter()
                            .map(|slot| &row[*slot])
                            .find(|value| !value.is_null())
                            .cloned()
                            .unwrap_or(Value::Null);
                        if value.is_null() {
                            Ok(value)
                        } else {
                            coercion::coerce(
                                value.clone(),
                                value
                                    .get_base_type()
                                    .expect("non-null value has a base type"),
                                *data_type,
                                CastContext::Implicit,
                                &context.get_timezone(),
                            )
                        }
                    }
                    ProjectionSource::Expression(expression) => evaluate_select_expression(
                        state,
                        &materialize_window_expression(
                            expression,
                            functions,
                            window_values,
                            AggregateOwner::Projection(index),
                        ),
                        scope,
                        row,
                        None,
                        xid,
                        snapshot,
                        context,
                    ),
                })
                .collect::<Result<Vec<_>>>()?;
            let keys = order_specs
                .iter()
                .enumerate()
                .map(|(index, order)| match order.key {
                    OrderKey::Output(index) => Ok(values[index].clone()),
                    OrderKey::Input(slot, _) => Ok(row[slot].clone()),
                    OrderKey::Expression(expression) => evaluate_select_expression(
                        state,
                        &materialize_window_expression(
                            expression,
                            functions,
                            window_values,
                            AggregateOwner::Order(index),
                        ),
                        scope,
                        row,
                        None,
                        xid,
                        snapshot,
                        context,
                    ),
                })
                .collect::<Result<Vec<_>>>()?;
            let distinct_keys = match distinct {
                DistinctPlan::On { keys: distinct, .. } => distinct
                    .iter()
                    .enumerate()
                    .map(|(index, key)| match key {
                        DistinctKey::Output(index) => Ok(values[*index].clone()),
                        DistinctKey::Order(index) => Ok(keys[*index].clone()),
                        DistinctKey::Expression(expression) => evaluate_select_expression(
                            state,
                            &materialize_window_expression(
                                expression,
                                functions,
                                window_values,
                                AggregateOwner::Distinct(index),
                            ),
                            scope,
                            row,
                            None,
                            xid,
                            snapshot,
                            context,
                        ),
                    })
                    .collect::<Result<Vec<_>>>()?,
                DistinctPlan::None | DistinctPlan::Rows => Vec::new(),
            };
            Ok(SelectRow {
                origins: Vec::new(),
                values,
                keys,
                distinct_keys,
                deferred_source: None,
                evaluated_projections: None,
            })
        })
        .collect()
}

impl ast::VisitorMut for WindowCollector {
    type Break = ();

    fn pre_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth -= 1;
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if self.query_depth == 0
            && let ast::Expr::Function(function) = expression
            && function.over.is_some()
            && (contains_volatile_expression(&ast::Expr::Function(function.clone()))
                || !self
                    .functions
                    .iter()
                    .any(|(candidate, _)| candidate == function))
        {
            self.functions.push((function.clone(), self.owner));
        }
        std::ops::ControlFlow::Continue(())
    }
}

impl ast::VisitorMut for WindowMaterializer<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth -= 1;
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if self.query_depth == 0
            && let ast::Expr::Function(function) = expression
        {
            let indexes = self
                .functions
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate.original == *function && candidate.owner == self.owner
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let count = self.seen.entry(function.clone()).or_default();
            let index = indexes
                .get(if indexes.len() == 1 { 0 } else { *count })
                .copied();
            if indexes.len() > 1 {
                *count += 1;
            }
            if let Some(index) = index {
                *expression = crate::analyzer::create_typed_literal(
                    self.values[index].clone(),
                    PgType::create(self.functions[index].data_type),
                );
            }
        }
        std::ops::ControlFlow::Continue(())
    }
}
