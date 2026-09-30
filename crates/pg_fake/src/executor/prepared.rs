use crate::{
    ColumnMeta,
    catalog::{TableId, TableSchema},
    coercion,
    database::DatabaseState,
    error::{PgError, Result, SqlState},
    executor::{
        StatementContext,
        aggregates::{
            AggregateDescriptor, AggregateInput, AggregateState, is_aggregate_function,
            parse_aggregate_call,
        },
        arithmetic::{evaluate_boolean_operator, evaluate_numeric_operator},
        expressions::{
            evaluate_comparison, evaluate_literal, infer_expression_type, is_null_literal,
        },
        normalize_identifier, normalize_relation_name,
        query::describe_query_result_columns,
        scope::{BoundScope, RowScope, bind_query_scope},
    },
    txn::{Snapshot, Xid, find_visible_version},
    value::{BaseType, PgType, Value},
};
use sqlparser::ast;
use std::time::Instant;

#[derive(Debug, Clone)]
pub(crate) struct PreparedQueryPlan {
    source: PreparedSource,
    output: PreparedOutput,
    selection: Option<PreparedExpression>,
    columns: Vec<ColumnMeta>,
    ordering: Vec<super::query::RowOrderSpec<'static>>,
    limit: Option<usize>,
    offset: usize,
}

impl PreparedQueryPlan {
    pub(crate) fn columns(&self) -> &[ColumnMeta] {
        &self.columns
    }
}

#[derive(Debug, Clone)]
enum PreparedAccess {
    Scan,
    Unique {
        column: usize,
        value: PreparedExpression,
    },
}

#[derive(Debug, Clone)]
enum PreparedSource {
    Table {
        table_id: TableId,
        access: PreparedAccess,
    },
    CteRows {
        id: usize,
    },
    StreamedJoin {
        table: ast::TableWithJoins,
        scope: BoundScope,
    },
}

#[derive(Debug, Clone)]
enum PreparedOutput {
    Rows(Vec<PreparedProjection>),
    Aggregates(Vec<PreparedAggregate>),
}

#[derive(Debug, Clone)]
struct PreparedAggregate {
    descriptor: AggregateDescriptor,
    argument: Option<PreparedExpression>,
}

#[derive(Debug, Clone)]
enum PreparedProjection {
    Column(usize),
    Expression(PreparedExpression),
}

#[derive(Debug, Clone)]
pub(super) enum PreparedExpression {
    Column {
        slot: usize,
        data_type: BaseType,
    },
    Parameter {
        index: usize,
        data_type: BaseType,
    },
    Literal {
        value: Value,
        data_type: BaseType,
    },
    Binary {
        left: Box<PreparedExpression>,
        operator: ast::BinaryOperator,
        left_constant: bool,
        right_constant: bool,
        right: Box<PreparedExpression>,
        data_type: BaseType,
    },
    Cast {
        expression: Box<PreparedExpression>,
        target: PgType,
    },
    Scalar {
        name: String,
        arguments: Vec<PreparedExpression>,
        targets: Vec<BaseType>,
        data_type: BaseType,
    },
    NullTest {
        expression: Box<PreparedExpression>,
        negated: bool,
    },
}

impl PreparedExpression {
    pub(super) fn get_data_type(&self) -> BaseType {
        match self {
            Self::Column { data_type, .. }
            | Self::Parameter { data_type, .. }
            | Self::Literal { data_type, .. } => *data_type,
            Self::Binary { data_type, .. } | Self::Scalar { data_type, .. } => *data_type,
            Self::Cast { target, .. } => target.base,
            Self::NullTest { .. } => BaseType::Bool,
        }
    }

    fn has_column(&self) -> bool {
        match self {
            Self::Column { .. } => true,
            Self::Parameter { .. } | Self::Literal { .. } => false,
            Self::Binary { left, right, .. } => left.has_column() || right.has_column(),
            Self::NullTest { expression, .. } | Self::Cast { expression, .. } => {
                expression.has_column()
            }
            Self::Scalar { arguments, .. } => arguments.iter().any(Self::has_column),
        }
    }

    pub(super) fn is_constant(&self) -> bool {
        match self {
            Self::Literal { .. } => true,
            Self::Column { .. }
            | Self::Parameter { .. }
            | Self::Cast { .. }
            | Self::Scalar { .. } => false,
            Self::Binary { left, right, .. } => left.is_constant() && right.is_constant(),
            Self::NullTest { expression, .. } => expression.is_constant(),
        }
    }
}

pub(crate) fn build_prepared_query_plan(
    state: &DatabaseState,
    statement: &ast::Statement,
    parameter_types: &[BaseType],
    described_columns: Option<&[ColumnMeta]>,
) -> Result<Option<PreparedQueryPlan>> {
    let ast::Statement::Query(query) = statement else {
        return Ok(None);
    };
    if query.with.is_some()
        || query.fetch.is_some()
        || !query.locks.is_empty()
        || query.for_clause.is_some()
        || query.settings.is_some()
        || query.format_clause.is_some()
        || !query.pipe_operators.is_empty()
    {
        return Ok(None);
    }
    let mut counts = [None, None];
    if let Some(clause) = &query.limit_clause {
        let ast::LimitClause::LimitOffset {
            limit,
            offset,
            limit_by,
        } = clause
        else {
            return Ok(None);
        };
        if !limit_by.is_empty() {
            return Ok(None);
        }
        for (index, expression) in [limit.as_ref(), offset.as_ref().map(|offset| &offset.value)]
            .into_iter()
            .enumerate()
        {
            let Some(expression) = expression else {
                continue;
            };
            match expression {
                ast::Expr::Value(value) => match &value.value {
                    ast::Value::Null => {}
                    ast::Value::Number(number, _)
                        if number.bytes().all(|byte| byte.is_ascii_digit()) =>
                    {
                        let Ok(value) = number.parse::<i64>() else {
                            return Ok(None);
                        };
                        counts[index] = Some(usize::try_from(value).unwrap_or(usize::MAX));
                    }
                    _ => return Ok(None),
                },
                ast::Expr::Identifier(ident)
                    if index == 0
                        && ident.quote_style.is_none()
                        && ident.value.eq_ignore_ascii_case("all") => {}
                _ => return Ok(None),
            }
        }
    }
    let [limit, offset] = counts;
    let offset = offset.unwrap_or(0);
    if limit == Some(0) || (query.limit_clause.is_some() && query.order_by.is_none()) {
        return Ok(None);
    }
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        return Ok(None);
    };
    let ast::GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
        return Ok(None);
    };
    if !select.optimizer_hints.is_empty()
        || select.distinct.is_some()
        || select.select_modifiers.is_some()
        || select.top.is_some()
        || select.exclude.is_some()
        || select.into.is_some()
        || !select.lateral_views.is_empty()
        || select.prewhere.is_some()
        || !select.connect_by.is_empty()
        || !group_by.is_empty()
        || !modifiers.is_empty()
        || !select.cluster_by.is_empty()
        || !select.distribute_by.is_empty()
        || !select.sort_by.is_empty()
        || select.having.is_some()
        || !select.named_window.is_empty()
        || select.qualify.is_some()
        || select.value_table_mode.is_some()
        || select.from.len() != 1
    {
        return Ok(None);
    }
    let ast::TableFactor::Table {
        name,
        alias: _,
        args: None,
        ..
    } = &select.from[0].relation
    else {
        return Ok(None);
    };
    let aggregate_query = !select.projection.is_empty()
        && select.projection.iter().all(|item| {
            matches!(item,
            ast::SelectItem::UnnamedExpr(ast::Expr::Function(function))
            | ast::SelectItem::ExprWithAlias { expr: ast::Expr::Function(function), .. }
            if is_aggregate_function(function))
        });
    if !(aggregate_query
        || select.projection.iter().all(|item| match item {
            ast::SelectItem::Wildcard(options) => {
                options == &ast::WildcardAdditionalOptions::default()
            }
            ast::SelectItem::UnnamedExpr(expression)
            | ast::SelectItem::ExprWithAlias {
                expr: expression, ..
            } => is_prepared_expression_candidate(expression),
            _ => false,
        }))
        || select
            .selection
            .as_ref()
            .is_some_and(|expression| !is_prepared_expression_candidate(expression))
    {
        return Ok(None);
    }
    let cte_row_source = crate::executor::ctes::cte_row_source_id(name);
    if !select.from[0].joins.is_empty()
        && (cte_row_source.is_none()
            || select.from[0].joins.len() != 1
            || !crate::executor::from::can_stream_join(&select.from[0])
            || !matches!(
                select.from[0].joins[0].join_operator,
                ast::JoinOperator::Join(ast::JoinConstraint::On(ast::Expr::BinaryOp {
                    op: ast::BinaryOperator::Eq,
                    ..
                })) | ast::JoinOperator::Inner(ast::JoinConstraint::On(ast::Expr::BinaryOp {
                    op: ast::BinaryOperator::Eq,
                    ..
                }))
            ))
    {
        return Ok(None);
    }
    let schema = if cte_row_source.is_some() {
        None
    } else {
        let relation_name = normalize_relation_name(name)?;
        if super::describe_visible_system_relation(&state.catalog, &relation_name).is_some() {
            return Ok(None);
        }
        if state.catalog.require_named_view(&relation_name).is_ok() {
            return Ok(None);
        }
        Some(state.catalog.require_named_table(&relation_name)?)
    };
    let scope = bind_query_scope(&state.catalog, select)?;
    if aggregate_query && described_columns.is_none() {
        if schema.is_none() {
            return Ok(None);
        }
        super::query::validate_select_predicates(state, select, &scope)?;
    }
    let mut columns = Vec::new();
    let output = if aggregate_query {
        let mut aggregates = Vec::new();
        for item in &select.projection {
            let function = match item {
                ast::SelectItem::UnnamedExpr(ast::Expr::Function(function))
                | ast::SelectItem::ExprWithAlias {
                    expr: ast::Expr::Function(function),
                    ..
                } => function,
                _ => unreachable!("aggregate projection was checked"),
            };
            if function.filter.is_some()
                || function.over.is_some()
                || function.null_treatment.is_some()
                || !function.within_group.is_empty()
                || function.uses_odbc_syntax
                || !matches!(function.parameters, ast::FunctionArguments::None)
            {
                return Ok(None);
            }
            let mut typed = function.clone();
            let ast::FunctionArguments::List(arguments) = &mut typed.args else {
                return Ok(None);
            };
            if !arguments.clauses.is_empty()
                || matches!(
                    arguments.duplicate_treatment,
                    Some(ast::DuplicateTreatment::Distinct)
                )
            {
                return Ok(None);
            }
            let argument = match arguments.args.as_mut_slice() {
                [] | [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Wildcard)] => None,
                [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expression))] => {
                    if is_null_literal(expression) {
                        return Ok(None);
                    }
                    let Some(argument) =
                        bind_prepared_expression(expression, &scope, parameter_types)?
                    else {
                        return Ok(None);
                    };
                    *expression = crate::analyzer::create_typed_literal(
                        Value::Null,
                        PgType::create(argument.get_data_type()),
                    );
                    Some(argument)
                }
                _ => return Ok(None),
            };
            let call = parse_aggregate_call(&typed, RowScope::Bound(&scope))?;
            aggregates.push(PreparedAggregate {
                descriptor: call.descriptor,
                argument,
            });
        }
        columns = match described_columns {
            Some(columns) => columns.to_vec(),
            None => describe_query_result_columns(state, statement)?,
        };
        PreparedOutput::Aggregates(aggregates)
    } else {
        let mut projection = Vec::new();
        for item in &select.projection {
            match item {
                ast::SelectItem::Wildcard(options)
                    if options == &ast::WildcardAdditionalOptions::default() =>
                {
                    for column in scope.columns.iter().filter(|column| column.wildcard) {
                        projection.push(PreparedProjection::Column(column.slot));
                        columns.push(ColumnMeta {
                            name: column.name.clone(),
                            type_oid: column.data_type.map_to_oid(),
                            typmod: column.data_type.typmod,
                        });
                    }
                }
                ast::SelectItem::UnnamedExpr(ast::Expr::Identifier(column)) => {
                    let (slot, data_type) = scope.resolve_column(std::slice::from_ref(column))?;
                    projection.push(PreparedProjection::Column(slot));
                    columns.push(ColumnMeta {
                        name: normalize_identifier(column),
                        type_oid: data_type.map_to_oid(),
                        typmod: data_type.typmod,
                    });
                }
                ast::SelectItem::UnnamedExpr(ast::Expr::CompoundIdentifier(identifiers)) => {
                    let (slot, data_type) = scope.resolve_column(identifiers)?;
                    projection.push(PreparedProjection::Column(slot));
                    columns.push(ColumnMeta {
                        name: normalize_identifier(
                            identifiers
                                .last()
                                .expect("compound identifier is non-empty"),
                        ),
                        type_oid: data_type.map_to_oid(),
                        typmod: data_type.typmod,
                    });
                }
                ast::SelectItem::ExprWithAlias {
                    expr: ast::Expr::Identifier(column),
                    alias,
                } => {
                    let (slot, data_type) = scope.resolve_column(std::slice::from_ref(column))?;
                    projection.push(PreparedProjection::Column(slot));
                    columns.push(ColumnMeta {
                        name: normalize_identifier(alias),
                        type_oid: data_type.map_to_oid(),
                        typmod: data_type.typmod,
                    });
                }
                ast::SelectItem::ExprWithAlias {
                    expr: ast::Expr::CompoundIdentifier(identifiers),
                    alias,
                } => {
                    let (slot, data_type) = scope.resolve_column(identifiers)?;
                    projection.push(PreparedProjection::Column(slot));
                    columns.push(ColumnMeta {
                        name: normalize_identifier(alias),
                        type_oid: data_type.map_to_oid(),
                        typmod: data_type.typmod,
                    });
                }
                ast::SelectItem::UnnamedExpr(expression) => {
                    let original = expression;
                    let Some(expression) =
                        bind_prepared_expression(expression, &scope, parameter_types)?
                    else {
                        return Ok(None);
                    };
                    let data_type = expression.get_data_type();
                    projection.push(PreparedProjection::Expression(expression));
                    columns.push(ColumnMeta {
                        name: super::query::get_projection_name(
                            original,
                            PgType::create(data_type),
                        )?,
                        type_oid: data_type.map_to_oid(),
                        typmod: PgType::NO_TYPEMOD,
                    });
                }
                ast::SelectItem::ExprWithAlias {
                    expr: expression,
                    alias,
                } => {
                    let Some(expression) =
                        bind_prepared_expression(expression, &scope, parameter_types)?
                    else {
                        return Ok(None);
                    };
                    let data_type = expression.get_data_type();
                    projection.push(PreparedProjection::Expression(expression));
                    columns.push(ColumnMeta {
                        name: normalize_identifier(alias),
                        type_oid: data_type.map_to_oid(),
                        typmod: PgType::NO_TYPEMOD,
                    });
                }
                _ => return Ok(None),
            }
        }
        PreparedOutput::Rows(projection)
    };
    let mut ordering = Vec::new();
    if query.order_by.is_some() || query.limit_clause.is_some() {
        let PreparedOutput::Rows(projection) = &output else {
            return Ok(None);
        };
        if (limit.is_some() || offset != 0)
            && !projection
                .iter()
                .all(|projection| matches!(projection, PreparedProjection::Column(_)))
        {
            return Ok(None);
        }
        if let Some(order_by) = &query.order_by {
            let ast::OrderByKind::Expressions(orders) = &order_by.kind else {
                return Ok(None);
            };
            if !orders.iter().all(|order| match &order.expr {
                ast::Expr::Identifier(ident) => columns.iter().any(|column| column.name == normalize_identifier(ident)),
                ast::Expr::Value(value) => matches!(&value.value, ast::Value::Number(number, _) if number.bytes().all(|byte| byte.is_ascii_digit())),
                _ => false,
            }) { return Ok(None); }
            let mut sources = Vec::new();
            for item in &select.projection {
                match item {
                    ast::SelectItem::Wildcard(_) => sources.extend(
                        scope
                            .columns
                            .iter()
                            .filter(|column| column.wildcard)
                            .map(|column| super::query::ProjectionSource::Column(column.slot)),
                    ),
                    ast::SelectItem::UnnamedExpr(expression)
                    | ast::SelectItem::ExprWithAlias {
                        expr: expression, ..
                    } => sources.push(super::query::ProjectionSource::Expression(expression)),
                    _ => return Ok(None),
                }
            }
            for spec in super::query::resolve_order_specs(state, query, &sources, &columns, &scope)?
            {
                let super::query::OrderKey::Output(output_slot) = spec.key else {
                    return Ok(None);
                };
                let PreparedProjection::Column(source_slot) = projection[output_slot] else {
                    return Ok(None);
                };
                ordering.push(super::query::RowOrderSpec {
                    key: super::query::OrderKey::Output(source_slot),
                    ascending: spec.ascending,
                    nulls_first: spec.nulls_first,
                });
            }
        }
    }
    let selection = match &select.selection {
        Some(selection) => {
            let Some(selection) = bind_prepared_expression(selection, &scope, parameter_types)?
            else {
                return Ok(None);
            };
            if selection.get_data_type() != BaseType::Bool {
                return Ok(None);
            }
            Some(selection)
        }
        None => None,
    };
    let source = if !select.from[0].joins.is_empty() {
        PreparedSource::StreamedJoin {
            table: select.from[0].clone(),
            scope: scope.clone(),
        }
    } else {
        match (cte_row_source, schema) {
            (Some(id), None) => PreparedSource::CteRows { id },
            (None, Some(schema)) => {
                let access = selection
                    .as_ref()
                    .and_then(|selection| find_unique_access(selection, schema))
                    .unwrap_or(PreparedAccess::Scan);
                PreparedSource::Table {
                    table_id: schema.id,
                    access,
                }
            }
            _ => unreachable!("CTE row source and table schema are mutually exclusive"),
        }
    };
    Ok(Some(PreparedQueryPlan {
        source,
        output,
        selection,
        columns,
        ordering,
        limit,
        offset,
    }))
}

fn is_prepared_expression_candidate(expression: &ast::Expr) -> bool {
    match expression {
        ast::Expr::Identifier(_) | ast::Expr::CompoundIdentifier(_) | ast::Expr::Value(_) => true,
        ast::Expr::Nested(expression) => is_prepared_expression_candidate(expression),
        ast::Expr::Function(_) | ast::Expr::Floor { .. } => true,
        ast::Expr::Cast {
            kind: ast::CastKind::Cast | ast::CastKind::DoubleColon,
            expr,
            format: None,
            ..
        } => is_prepared_expression_candidate(expr),
        ast::Expr::IsNull(expression) | ast::Expr::IsNotNull(expression) => {
            is_prepared_expression_candidate(expression)
        }
        ast::Expr::BinaryOp {
            left,
            op:
                ast::BinaryOperator::Eq
                | ast::BinaryOperator::NotEq
                | ast::BinaryOperator::Gt
                | ast::BinaryOperator::Lt
                | ast::BinaryOperator::GtEq
                | ast::BinaryOperator::LtEq
                | ast::BinaryOperator::And
                | ast::BinaryOperator::Or
                | ast::BinaryOperator::Plus
                | ast::BinaryOperator::Minus
                | ast::BinaryOperator::Multiply
                | ast::BinaryOperator::Divide
                | ast::BinaryOperator::Modulo,
            right,
        } => is_prepared_expression_candidate(left) && is_prepared_expression_candidate(right),
        _ => false,
    }
}

pub(super) fn bind_prepared_expression(
    expression: &ast::Expr,
    scope: &BoundScope,
    parameter_types: &[BaseType],
) -> Result<Option<PreparedExpression>> {
    match expression {
        ast::Expr::Identifier(column) => {
            let (slot, data_type) = scope.resolve_column(std::slice::from_ref(column))?;
            Ok(Some(PreparedExpression::Column {
                slot,
                data_type: data_type.base,
            }))
        }
        ast::Expr::CompoundIdentifier(columns) => {
            let (slot, data_type) = scope.resolve_column(columns)?;
            Ok(Some(PreparedExpression::Column {
                slot,
                data_type: data_type.base,
            }))
        }
        ast::Expr::Value(value) => match &value.value {
            ast::Value::Placeholder(placeholder) => {
                let index = crate::analyzer::parse_placeholder_index(placeholder)?;
                let Some(data_type) = parameter_types.get(index).copied() else {
                    return Ok(None);
                };
                Ok(Some(PreparedExpression::Parameter { index, data_type }))
            }
            ast::Value::SingleQuotedString(_) => Ok(None),
            _ => Ok(Some(PreparedExpression::Literal {
                value: evaluate_literal(expression)?,
                data_type: infer_expression_type(expression, RowScope::Bound(scope))?,
            })),
        },
        ast::Expr::Nested(expression) => {
            bind_prepared_expression(expression, scope, parameter_types)
        }
        ast::Expr::Cast {
            kind: ast::CastKind::Cast | ast::CastKind::DoubleColon,
            expr,
            data_type,
            format: None,
        } => {
            let Some(expression) = bind_prepared_expression(expr, scope, parameter_types)? else {
                return Ok(None);
            };
            let target = coercion::convert_ast_data_type(data_type)?;
            if target.typmod != PgType::NO_TYPEMOD {
                return Ok(None);
            }
            if target == PgType::create(expression.get_data_type()) {
                return Ok(Some(expression));
            }
            if !super::expressions::is_numeric_type(expression.get_data_type())
                || !super::expressions::is_numeric_type(target.base)
            {
                return Ok(None);
            }
            Ok(Some(PreparedExpression::Cast {
                expression: Box::new(expression),
                target,
            }))
        }
        ast::Expr::Function(_) | ast::Expr::Floor { .. } => {
            let (name, arguments): (String, Vec<&ast::Expr>) = match expression {
                ast::Expr::Floor {
                    expr,
                    field: ast::CeilFloorKind::DateTimeField(ast::DateTimeField::NoDateTime),
                } => ("floor".into(), vec![expr]),
                ast::Expr::Floor { .. } => return Ok(None),
                ast::Expr::Function(function) => {
                    let name = super::normalize_function_name(&function.name)?;
                    if !matches!(
                        name.as_str(),
                        "floor" | "to_timestamp" | "to_char" | "date_trunc"
                    ) || function.filter.is_some()
                        || function.over.is_some()
                        || function.null_treatment.is_some()
                        || !function.within_group.is_empty()
                        || function.uses_odbc_syntax
                        || !matches!(function.parameters, ast::FunctionArguments::None)
                    {
                        return Ok(None);
                    }
                    let ast::FunctionArguments::List(arguments) = &function.args else {
                        return Ok(None);
                    };
                    if arguments.duplicate_treatment.is_some() || !arguments.clauses.is_empty() {
                        return Ok(None);
                    }
                    let Some(arguments) = arguments
                        .args
                        .iter()
                        .map(|arg| match arg {
                            ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expr)) => {
                                Some(expr)
                            }
                            _ => None,
                        })
                        .collect::<Option<Vec<_>>>()
                    else {
                        return Ok(None);
                    };
                    (name, arguments)
                }
                _ => unreachable!(),
            };
            let mut bound = Vec::new();
            let mut types = Vec::new();
            for argument in arguments {
                if let Some(text) = super::expressions::extract_unknown_string_literal(argument) {
                    bound.push(PreparedExpression::Literal {
                        value: Value::Text(text.into()),
                        data_type: BaseType::Text,
                    });
                    types.push(None);
                } else if is_null_literal(argument) {
                    bound.push(PreparedExpression::Literal {
                        value: Value::Null,
                        data_type: BaseType::Text,
                    });
                    types.push(None);
                } else {
                    let Some(argument) =
                        bind_prepared_expression(argument, scope, parameter_types)?
                    else {
                        return Ok(None);
                    };
                    types.push(Some(argument.get_data_type()));
                    bound.push(argument);
                }
            }
            let (targets, data_type) = super::expressions::resolve_runtime_function(&name, &types)
                .expect("recognized scalar")?;
            for ((argument, source), target) in bound.iter_mut().zip(types).zip(&targets) {
                if let Some(source) = source {
                    if !coercion::can_cast(source, *target, coercion::CastContext::Implicit) {
                        return Ok(None);
                    }
                } else {
                    let PreparedExpression::Literal { value, data_type } = argument else {
                        unreachable!()
                    };
                    if let Value::Text(text) = value {
                        if !matches!(
                            target,
                            BaseType::Text | BaseType::Float8 | BaseType::Numeric
                        ) {
                            return Ok(None);
                        }
                        *value = coercion::coerce_unknown(
                            text,
                            PgType::create(*target),
                            coercion::CastContext::Implicit,
                            "UTC",
                        )?;
                    }
                    *data_type = *target;
                }
            }
            Ok(Some(PreparedExpression::Scalar {
                name,
                arguments: bound,
                targets,
                data_type,
            }))
        }

        ast::Expr::IsNull(operand) | ast::Expr::IsNotNull(operand) => {
            let Some(operand) = bind_prepared_expression(operand, scope, parameter_types)? else {
                return Ok(None);
            };
            Ok(Some(PreparedExpression::NullTest {
                expression: Box::new(operand),
                negated: matches!(expression, ast::Expr::IsNotNull(_)),
            }))
        }
        ast::Expr::BinaryOp {
            left,
            op:
                operator @ (ast::BinaryOperator::Eq
                | ast::BinaryOperator::NotEq
                | ast::BinaryOperator::Gt
                | ast::BinaryOperator::Lt
                | ast::BinaryOperator::GtEq
                | ast::BinaryOperator::LtEq
                | ast::BinaryOperator::And
                | ast::BinaryOperator::Or
                | ast::BinaryOperator::Plus
                | ast::BinaryOperator::Minus
                | ast::BinaryOperator::Multiply
                | ast::BinaryOperator::Divide
                | ast::BinaryOperator::Modulo),
            right,
        } => {
            let Some(mut left_expression) = bind_prepared_expression(left, scope, parameter_types)?
            else {
                return Ok(None);
            };
            let Some(mut right_expression) =
                bind_prepared_expression(right, scope, parameter_types)?
            else {
                return Ok(None);
            };
            let data_type =
                if matches!(operator, ast::BinaryOperator::And | ast::BinaryOperator::Or) {
                    if left_expression.get_data_type() != BaseType::Bool
                        || right_expression.get_data_type() != BaseType::Bool
                    {
                        return Ok(None);
                    }
                    BaseType::Bool
                } else {
                    if left_expression.get_data_type() != right_expression.get_data_type() {
                        if !matches!(
                            operator,
                            ast::BinaryOperator::Plus
                                | ast::BinaryOperator::Minus
                                | ast::BinaryOperator::Multiply
                                | ast::BinaryOperator::Divide
                                | ast::BinaryOperator::Modulo
                        ) || !super::expressions::is_numeric_type(
                            left_expression.get_data_type(),
                        ) || !super::expressions::is_numeric_type(
                            right_expression.get_data_type(),
                        ) {
                            return Ok(None);
                        }
                        let target = super::expressions::resolve_operator_type(
                            &crate::analyzer::create_typed_literal(
                                Value::Null,
                                PgType::create(left_expression.get_data_type()),
                            ),
                            &crate::analyzer::create_typed_literal(
                                Value::Null,
                                PgType::create(right_expression.get_data_type()),
                            ),
                            RowScope::Bound(scope),
                        )?;
                        left_expression = PreparedExpression::Cast {
                            expression: Box::new(left_expression),
                            target: PgType::create(target),
                        };
                        right_expression = PreparedExpression::Cast {
                            expression: Box::new(right_expression),
                            target: PgType::create(target),
                        };
                    }
                    if matches!(
                        operator,
                        ast::BinaryOperator::Plus
                            | ast::BinaryOperator::Minus
                            | ast::BinaryOperator::Multiply
                            | ast::BinaryOperator::Divide
                            | ast::BinaryOperator::Modulo
                    ) {
                        let data_type = left_expression.get_data_type();
                        if !super::expressions::is_numeric_type(data_type) {
                            return Ok(None);
                        }
                        data_type
                    } else {
                        BaseType::Bool
                    }
                };
            Ok(Some(PreparedExpression::Binary {
                left: Box::new(left_expression),
                operator: operator.clone(),
                left_constant: super::expressions::is_constant_expression(left),
                right_constant: super::expressions::is_constant_expression(right),
                right: Box::new(right_expression),
                data_type,
            }))
        }
        _ => Ok(None),
    }
}

fn find_unique_access(
    selection: &PreparedExpression,
    schema: &TableSchema,
) -> Option<PreparedAccess> {
    let PreparedExpression::Binary {
        left,
        operator: ast::BinaryOperator::Eq,
        right,
        ..
    } = selection
    else {
        return None;
    };
    let (column, value) = match (left.as_ref(), right.as_ref()) {
        (PreparedExpression::Column { slot, .. }, value) if !value.has_column() => {
            (*slot, value.clone())
        }
        (value, PreparedExpression::Column { slot, .. }) if !value.has_column() => {
            (*slot, value.clone())
        }
        _ => return None,
    };
    schema
        .constraints
        .iter()
        .any(|constraint| match constraint {
            crate::catalog::Constraint::PrimaryKey { columns, .. }
            | crate::catalog::Constraint::Unique { columns, .. } => {
                columns.len() == 1 && columns[0] == schema.columns[column].name
            }
            _ => false,
        })
        .then_some(PreparedAccess::Unique { column, value })
}

pub(crate) fn execute_prepared_query(
    state: &DatabaseState,
    plan: &PreparedQueryPlan,
    parameters: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: Option<&StatementContext>,
    deadline: Option<Instant>,
    timezone: &str,
) -> Result<Vec<Vec<Value>>> {
    let mut rows = Vec::new();
    let mut aggregate_states = match &plan.output {
        PreparedOutput::Rows(_) => Vec::new(),
        PreparedOutput::Aggregates(aggregates) => aggregates
            .iter()
            .map(|aggregate| AggregateState::create(&aggregate.descriptor))
            .collect::<Vec<_>>(),
    };
    let defer_projection = !plan.ordering.is_empty() || plan.limit.is_some() || plan.offset != 0;
    let project = |row: &[Value]| -> Result<Vec<Value>> {
        let PreparedOutput::Rows(projection) = &plan.output else {
            unreachable!("row projection requires row output");
        };
        projection
            .iter()
            .map(|projection| match projection {
                PreparedProjection::Column(slot) => Ok(row[*slot].clone()),
                PreparedProjection::Expression(expression) => {
                    evaluate_prepared_expression(expression, row, parameters, deadline, timezone)
                }
            })
            .collect()
    };
    let compare = |left: &Vec<Value>, right: &Vec<Value>| {
        plan.ordering
            .iter()
            .map(|spec| {
                let super::query::OrderKey::Output(slot) = spec.key else {
                    unreachable!("prepared ordering uses source slots");
                };
                super::query::compare_order_keys(
                    std::slice::from_ref(&left[slot]),
                    std::slice::from_ref(&right[slot]),
                    std::slice::from_ref(spec),
                )
            })
            .find(|ordering| !ordering.is_eq())
            .unwrap_or(std::cmp::Ordering::Equal)
    };
    let top_k = plan.limit.map(|limit| plan.offset.saturating_add(limit));
    let mut visit = |row: &[Value]| -> Result<()> {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(PgError::create(
                SqlState::QueryCanceled,
                "canceling statement due to statement timeout",
            ));
        }
        if let Some(selection) = &plan.selection
            && !matches!(
                evaluate_prepared_expression(selection, row, parameters, deadline, timezone)?,
                Value::Bool(true)
            )
        {
            return Ok(());
        }
        match &plan.output {
            PreparedOutput::Rows(_) => {
                if defer_projection {
                    super::query::retain_top_ordered_row(&mut rows, row.to_vec(), top_k, &compare);
                } else {
                    rows.push(project(row)?);
                }
            }
            PreparedOutput::Aggregates(aggregates) => {
                for (aggregate, aggregate_state) in aggregates.iter().zip(&mut aggregate_states) {
                    let argument = aggregate
                        .argument
                        .as_ref()
                        .map(|expression| {
                            evaluate_prepared_expression(
                                expression, row, parameters, deadline, timezone,
                            )
                        })
                        .transpose()?;
                    aggregate_state.add_input(
                        &aggregate.descriptor,
                        AggregateInput {
                            included: true,
                            argument,
                            delimiter: None,
                            order_keys: Vec::new(),
                        },
                    );
                }
            }
        }
        Ok(())
    };
    match &plan.source {
        PreparedSource::CteRows { id } => {
            let source = context
                .expect("prepared CTE source requires its statement context")
                .get_cte_row_source(*id)
                .expect("prepared CTE row source was registered");
            for row in &source.rows {
                visit(row)?;
            }
        }
        PreparedSource::StreamedJoin { table, scope } => {
            crate::executor::from::visit_streamed_join_rows(
                state,
                table,
                scope,
                xid,
                snapshot,
                context.expect("prepared recursive join requires its statement context"),
                None,
                &mut |row| visit(row),
            )?;
        }
        PreparedSource::Table { table_id, access } => {
            state.catalog.require_table_by_id(*table_id)?;
            let table = state
                .tables
                .get(table_id)
                .expect("prepared table must have storage");
            match access {
                PreparedAccess::Scan => {
                    state.record_read(xid, crate::serializable::Access::Relation(*table_id));
                    for (row_id, chain) in table.iterate_version_chains() {
                        if let Some(version) =
                            find_visible_version(chain, snapshot, xid, &state.transactions)
                        {
                            state.record_read(
                                xid,
                                crate::serializable::Access::Row(*table_id, row_id),
                            );
                            visit(&version.row)?;
                        }
                    }
                }
                PreparedAccess::Unique { column, value } => {
                    let value =
                        evaluate_prepared_expression(value, &[], parameters, deadline, timezone)?;
                    if let Some(key) =
                        table.create_unique_read_key(&[*column], std::slice::from_ref(&value))
                    {
                        state.record_read(
                            xid,
                            crate::serializable::Access::Unique(*table_id, vec![*column], key),
                        );
                    }
                    if let Some((row_id, version)) = table.find_unique_visible_version(
                        &[*column],
                        &[value],
                        snapshot,
                        xid,
                        &state.transactions,
                    ) {
                        state.record_read(xid, crate::serializable::Access::Row(*table_id, row_id));
                        visit(&version.row)?;
                    }
                }
            }
        }
    }
    if defer_projection {
        rows.sort_by(compare);
        let projected = rows
            .iter()
            .map(|row| project(row))
            .collect::<Result<Vec<_>>>()?;
        return Ok(projected.into_iter().skip(plan.offset).collect());
    }
    if let PreparedOutput::Aggregates(aggregates) = &plan.output {
        rows.push(
            aggregate_states
                .into_iter()
                .zip(aggregates)
                .map(|(state, aggregate)| {
                    state.finish(&aggregate.descriptor).map(|(value, _)| value)
                })
                .collect::<Result<_>>()?,
        );
    }
    Ok(rows)
}

pub(super) fn evaluate_prepared_expression(
    expression: &PreparedExpression,
    row: &[Value],
    parameters: &[Value],
    deadline: Option<Instant>,
    timezone: &str,
) -> Result<Value> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(PgError::create(
            SqlState::QueryCanceled,
            "canceling statement due to statement timeout",
        ));
    }
    match expression {
        PreparedExpression::Column { slot, .. } => Ok(row[*slot].clone()),
        PreparedExpression::Parameter { index, .. } => Ok(parameters[*index].clone()),
        PreparedExpression::Literal { value, .. } => Ok(value.clone()),
        PreparedExpression::Binary {
            left,
            operator,
            right,
            left_constant,
            right_constant,
            ..
        } => {
            let boolean = matches!(operator, ast::BinaryOperator::And | ast::BinaryOperator::Or);
            let constant_left = if boolean && *left_constant {
                Some(evaluate_prepared_expression(
                    left, row, parameters, deadline, timezone,
                )?)
            } else {
                None
            };
            if matches!(
                (operator, &constant_left),
                (ast::BinaryOperator::And, Some(Value::Bool(false)))
                    | (ast::BinaryOperator::Or, Some(Value::Bool(true)))
            ) {
                return Ok(constant_left.expect("decisive constant was evaluated"));
            }
            let constant_right = if boolean && *right_constant {
                Some(evaluate_prepared_expression(
                    right, row, parameters, deadline, timezone,
                )?)
            } else {
                None
            };
            if matches!(
                (operator, &constant_right),
                (ast::BinaryOperator::And, Some(Value::Bool(false)))
                    | (ast::BinaryOperator::Or, Some(Value::Bool(true)))
            ) {
                return Ok(constant_right.expect("decisive constant was evaluated"));
            }
            let left_value = match constant_left {
                Some(value) => value,
                None => evaluate_prepared_expression(left, row, parameters, deadline, timezone)?,
            };
            if matches!(
                (operator, &left_value),
                (ast::BinaryOperator::And, Value::Bool(false))
                    | (ast::BinaryOperator::Or, Value::Bool(true))
            ) {
                return Ok(left_value);
            }
            let right_value = match constant_right {
                Some(value) => value,
                None => evaluate_prepared_expression(right, row, parameters, deadline, timezone)?,
            };
            match operator {
                ast::BinaryOperator::Eq
                | ast::BinaryOperator::NotEq
                | ast::BinaryOperator::Gt
                | ast::BinaryOperator::Lt
                | ast::BinaryOperator::GtEq
                | ast::BinaryOperator::LtEq => {
                    if left_value.is_null() || right_value.is_null() {
                        Ok(Value::Null)
                    } else {
                        evaluate_comparison(operator, &left_value, &right_value)
                    }
                }
                ast::BinaryOperator::And | ast::BinaryOperator::Or => {
                    evaluate_boolean_operator(operator, left_value, right_value)
                }
                ast::BinaryOperator::Plus
                | ast::BinaryOperator::Minus
                | ast::BinaryOperator::Multiply
                | ast::BinaryOperator::Divide
                | ast::BinaryOperator::Modulo => {
                    if left_value.is_null() || right_value.is_null() {
                        Ok(Value::Null)
                    } else {
                        evaluate_numeric_operator(operator, left_value, right_value)
                    }
                }
                _ => unreachable!("prepared expression only contains supported operators"),
            }
        }
        PreparedExpression::Cast { expression, target } => {
            let value =
                evaluate_prepared_expression(expression, row, parameters, deadline, timezone)?;
            coercion::coerce(
                value,
                expression.get_data_type(),
                *target,
                coercion::CastContext::Explicit,
                timezone,
            )
        }
        PreparedExpression::Scalar {
            name,
            arguments,
            targets,
            ..
        } => {
            let values = arguments
                .iter()
                .zip(targets)
                .map(|(argument, target)| {
                    let value = evaluate_prepared_expression(
                        argument, row, parameters, deadline, timezone,
                    )?;
                    coercion::coerce(
                        value,
                        argument.get_data_type(),
                        PgType::create(*target),
                        coercion::CastContext::Implicit,
                        timezone,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            super::expressions::evaluate_prepared_runtime_function(name, &values, timezone)
        }
        PreparedExpression::NullTest {
            expression,
            negated,
        } => Ok(Value::Bool(
            evaluate_prepared_expression(expression, row, parameters, deadline, timezone)?
                .is_null()
                != *negated,
        )),
    }
}
