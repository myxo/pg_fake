use super::expressions::evaluate_literal;
use super::*;

#[derive(Debug, Clone)]
pub(crate) struct PreparedQueryPlan {
    table_name: String,
    table_id: TableId,
    projection: Vec<usize>,
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
enum PreparedExpression {
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
    },
}

impl PreparedExpression {
    fn get_data_type(&self) -> BaseType {
        match self {
            Self::Column { data_type, .. }
            | Self::Parameter { data_type, .. }
            | Self::Literal { data_type, .. } => *data_type,
            Self::Binary { .. } => BaseType::Bool,
        }
    }

    fn has_column(&self) -> bool {
        match self {
            Self::Column { .. } => true,
            Self::Parameter { .. } | Self::Literal { .. } => false,
            Self::Binary { left, right, .. } => left.has_column() || right.has_column(),
        }
    }
}

pub(crate) fn build_prepared_query_plan(
    state: &DatabaseState,
    statement: &ast::Statement,
    parameter_types: &[BaseType],
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
        name, args: None, ..
    } = &select.from[0].relation
    else {
        return Ok(None);
    };
    if !select.projection.iter().all(|item| {
        matches!(
            item,
            ast::SelectItem::Wildcard(options)
                if options == &ast::WildcardAdditionalOptions::default()
        ) || matches!(
            item,
            ast::SelectItem::UnnamedExpr(
                ast::Expr::Identifier(_) | ast::Expr::CompoundIdentifier(_)
            ) | ast::SelectItem::ExprWithAlias {
                expr: ast::Expr::Identifier(_) | ast::Expr::CompoundIdentifier(_),
                ..
            }
        )
    }) {
        return Ok(None);
    }
    let table_name = normalize_unqualified_object_name(name)?;
    let schema = state.catalog.require_table(&table_name)?;
    let scope = bind_query_scope(&state.catalog, select)?;
    let mut projection = Vec::new();
    let mut columns = Vec::new();
    for item in &select.projection {
        match item {
            ast::SelectItem::Wildcard(options)
                if options == &ast::WildcardAdditionalOptions::default() =>
            {
                for column in scope.columns.iter().filter(|column| column.wildcard) {
                    projection.push(column.slot);
                    columns.push(ColumnMeta {
                        name: column.name.clone(),
                        type_oid: column.data_type.map_to_oid(),
                        typmod: column.data_type.typmod,
                    });
                }
            }
            ast::SelectItem::UnnamedExpr(ast::Expr::Identifier(column)) => {
                let (slot, data_type) = scope.resolve_column(std::slice::from_ref(column))?;
                projection.push(slot);
                columns.push(ColumnMeta {
                    name: column.value.clone(),
                    type_oid: data_type.map_to_oid(),
                    typmod: data_type.typmod,
                });
            }
            ast::SelectItem::UnnamedExpr(ast::Expr::CompoundIdentifier(identifiers)) => {
                let (slot, data_type) = scope.resolve_column(identifiers)?;
                projection.push(slot);
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
                projection.push(slot);
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
                projection.push(slot);
                columns.push(ColumnMeta {
                    name: alias.value.clone(),
                    type_oid: data_type.map_to_oid(),
                    typmod: data_type.typmod,
                });
            }
            _ => return Ok(None),
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
    let access = selection
        .as_ref()
        .and_then(|selection| find_unique_access(selection, schema))
        .unwrap_or(PreparedAccess::Scan);
    Ok(Some(PreparedQueryPlan {
        table_name,
        table_id: schema.id,
        projection,
        selection,
        access,
        columns,
    }))
}

fn bind_prepared_expression(
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
                | ast::BinaryOperator::Or),
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
            if matches!(operator, ast::BinaryOperator::And | ast::BinaryOperator::Or) {
                if left_expression.get_data_type() != BaseType::Bool
                    || right_expression.get_data_type() != BaseType::Bool
                {
                    return Ok(None);
                }
            } else {
                if left_expression.get_data_type() != right_expression.get_data_type() {
                    return Ok(None);
                }
            }
            Ok(Some(PreparedExpression::Binary {
                left: Box::new(left_expression),
                operator: operator.clone(),
                right: Box::new(right_expression),
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
            crate::catalog::Constraint::PrimaryKey(columns)
            | crate::catalog::Constraint::Unique(columns) => {
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
) -> Result<Vec<Vec<Value>>> {
    let schema = state.catalog.require_table(&plan.table_name)?;
    if schema.id != plan.table_id {
        return Err(PgError::create(
            SqlState::FeatureNotSupported,
            "cached plan must be replanned",
        ));
    }
    let table = state
        .tables
        .get(&plan.table_id)
        .expect("prepared table must have storage");
    let mut rows = Vec::new();
    let mut visit = |row: &[Value]| -> Result<()> {
        if let Some(selection) = &plan.selection
            && !matches!(
                evaluate_prepared_expression(selection, row, parameters)?,
                Value::Bool(true)
            )
        {
            return Ok(());
        }
        rows.push(
            plan.projection
                .iter()
                .map(|slot| row[*slot].clone())
                .collect(),
        );
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
            let value = evaluate_prepared_expression(value, &[], parameters)?;
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
    Ok(rows)
}

fn evaluate_prepared_expression(
    expression: &PreparedExpression,
    row: &[Value],
    parameters: &[Value],
) -> Result<Value> {
    match expression {
        PreparedExpression::Column { slot, .. } => Ok(row[*slot].clone()),
        PreparedExpression::Parameter { index, .. } => Ok(parameters[*index].clone()),
        PreparedExpression::Literal { value, .. } => Ok(value.clone()),
        PreparedExpression::Binary {
            left,
            operator,
            right,
        } => {
            let left_value = evaluate_prepared_expression(left, row, parameters)?;
            let right_value = evaluate_prepared_expression(right, row, parameters)?;
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
                _ => unreachable!("prepared expression only contains supported operators"),
            }
        }
    }
}
