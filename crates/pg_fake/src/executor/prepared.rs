use crate::{
    ColumnMeta,
    catalog::{TableId, TableSchema},
    coercion,
    database::DatabaseState,
    error::{PgError, Result, SqlState},
    executor::{
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
    table_id: TableId,
    output: PreparedOutput,
    selection: Option<PreparedExpression>,
    access: PreparedAccess,
    columns: Vec<ColumnMeta>,
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
        right: Box<PreparedExpression>,
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
            Self::Binary { data_type, .. } => *data_type,
            Self::NullTest { .. } => BaseType::Bool,
        }
    }

    fn has_column(&self) -> bool {
        match self {
            Self::Column { .. } => true,
            Self::Parameter { .. } | Self::Literal { .. } => false,
            Self::Binary { left, right, .. } => left.has_column() || right.has_column(),
            Self::NullTest { expression, .. } => expression.has_column(),
        }
    }

    pub(super) fn is_constant(&self) -> bool {
        match self {
            Self::Literal { .. } => true,
            Self::Column { .. } | Self::Parameter { .. } => false,
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
        || query.order_by.is_some()
        || query.limit_clause.is_some()
        || query.fetch.is_some()
        || !query.locks.is_empty()
        || query.for_clause.is_some()
        || query.settings.is_some()
        || query.format_clause.is_some()
        || !query.pipe_operators.is_empty()
    {
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
        || !select.from[0].joins.is_empty()
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
    let relation_name = normalize_relation_name(name)?;
    if super::describe_visible_system_relation(&state.catalog, &relation_name).is_some() {
        return Ok(None);
    }
    if state.catalog.require_named_view(&relation_name).is_ok() {
        return Ok(None);
    }
    let schema = state.catalog.require_named_table(&relation_name)?;
    let scope = bind_query_scope(&state.catalog, select)?;
    if aggregate_query && described_columns.is_none() {
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
                        name: column.value.clone(),
                        type_oid: data_type.map_to_oid(),
                        typmod: data_type.typmod,
                    });
                }
                ast::SelectItem::UnnamedExpr(ast::Expr::CompoundIdentifier(identifiers)) => {
                    let (slot, data_type) = scope.resolve_column(identifiers)?;
                    projection.push(PreparedProjection::Column(slot));
                    columns.push(ColumnMeta {
                        name: identifiers
                            .last()
                            .expect("compound identifier is non-empty")
                            .value
                            .clone(),
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
                        name: alias.value.clone(),
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
                        name: alias.value.clone(),
                        type_oid: data_type.map_to_oid(),
                        typmod: data_type.typmod,
                    });
                }
                ast::SelectItem::UnnamedExpr(expression) => {
                    let Some(expression) =
                        bind_prepared_expression(expression, &scope, parameter_types)?
                    else {
                        return Ok(None);
                    };
                    let data_type = expression.get_data_type();
                    projection.push(PreparedProjection::Expression(expression));
                    columns.push(ColumnMeta {
                        name: "?column?".into(),
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
    let access = selection
        .as_ref()
        .and_then(|selection| find_unique_access(selection, schema))
        .unwrap_or(PreparedAccess::Scan);
    Ok(Some(PreparedQueryPlan {
        table_id: schema.id,
        output,
        selection,
        access,
        columns,
    }))
}

fn is_prepared_expression_candidate(expression: &ast::Expr) -> bool {
    match expression {
        ast::Expr::Identifier(_) | ast::Expr::CompoundIdentifier(_) | ast::Expr::Value(_) => true,
        ast::Expr::Nested(expression) => is_prepared_expression_candidate(expression),
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
            let Some(parameter @ PreparedExpression::Parameter { .. }) =
                bind_prepared_expression(expr, scope, parameter_types)?
            else {
                return Ok(None);
            };
            let target = coercion::convert_ast_data_type(data_type)?;
            Ok((target == PgType::create(parameter.get_data_type())).then_some(parameter))
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
            let Some(left_expression) = bind_prepared_expression(left, scope, parameter_types)?
            else {
                return Ok(None);
            };
            let Some(right_expression) = bind_prepared_expression(right, scope, parameter_types)?
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
                        return Ok(None);
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
                        if !matches!(
                            data_type,
                            BaseType::Int2
                                | BaseType::Int4
                                | BaseType::Int8
                                | BaseType::Float4
                                | BaseType::Float8
                                | BaseType::Numeric
                        ) {
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
    deadline: Option<Instant>,
) -> Result<Vec<Vec<Value>>> {
    state.catalog.require_table_by_id(plan.table_id)?;
    let table = state
        .tables
        .get(&plan.table_id)
        .expect("prepared table must have storage");
    let mut rows = Vec::new();
    let mut aggregate_states = match &plan.output {
        PreparedOutput::Rows(_) => Vec::new(),
        PreparedOutput::Aggregates(aggregates) => aggregates
            .iter()
            .map(|aggregate| AggregateState::create(&aggregate.descriptor))
            .collect::<Vec<_>>(),
    };
    let mut visit = |row: &[Value]| -> Result<()> {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(PgError::create(
                SqlState::QueryCanceled,
                "canceling statement due to statement timeout",
            ));
        }
        if let Some(selection) = &plan.selection
            && !matches!(
                evaluate_prepared_expression(selection, row, parameters, deadline)?,
                Value::Bool(true)
            )
        {
            return Ok(());
        }
        match &plan.output {
            PreparedOutput::Rows(projection) => rows.push(
                projection
                    .iter()
                    .map(|projection| match projection {
                        PreparedProjection::Column(slot) => Ok(row[*slot].clone()),
                        PreparedProjection::Expression(expression) => {
                            evaluate_prepared_expression(expression, row, parameters, deadline)
                        }
                    })
                    .collect::<Result<_>>()?,
            ),
            PreparedOutput::Aggregates(aggregates) => {
                for (aggregate, aggregate_state) in aggregates.iter().zip(&mut aggregate_states) {
                    let argument = aggregate
                        .argument
                        .as_ref()
                        .map(|expression| {
                            evaluate_prepared_expression(expression, row, parameters, deadline)
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
    match &plan.access {
        PreparedAccess::Scan => {
            for (_, chain) in table.iterate_version_chains() {
                if let Some(version) =
                    find_visible_version(chain, snapshot, xid, &state.transactions)
                {
                    visit(&version.row)?;
                }
            }
        }
        PreparedAccess::Unique { column, value } => {
            let value = evaluate_prepared_expression(value, &[], parameters, deadline)?;
            if let Some(row) = table.find_unique_visible_row(
                &[*column],
                &[value],
                snapshot,
                xid,
                &state.transactions,
            ) {
                visit(row)?;
            }
        }
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
            ..
        } => {
            let left_value = evaluate_prepared_expression(left, row, parameters, deadline)?;
            if matches!(
                (operator, &left_value),
                (ast::BinaryOperator::And, Value::Bool(false))
                    | (ast::BinaryOperator::Or, Value::Bool(true))
            ) {
                return Ok(left_value);
            }
            let right_value = evaluate_prepared_expression(right, row, parameters, deadline)?;
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
        PreparedExpression::NullTest {
            expression,
            negated,
        } => Ok(Value::Bool(
            evaluate_prepared_expression(expression, row, parameters, deadline)?.is_null()
                != *negated,
        )),
    }
}
