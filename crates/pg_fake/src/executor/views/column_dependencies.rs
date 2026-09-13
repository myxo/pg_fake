use crate::catalog::{Catalog, TableId, ViewDependency};
use crate::executor::ctes::scope::{CteNameScope, enter_cte_scope};
use crate::executor::{
    normalize_identifier, normalize_relation_name, normalize_unqualified_object_name,
    scope::{self, infer_query_output_columns},
};
use sqlparser::ast::{self, VisitMut as _};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct ColumnReferenceScope {
    target_columns: BTreeSet<(String, String)>,
    target_names: BTreeMap<String, usize>,
    source_qualifiers: BTreeSet<String>,
    target_sources: usize,
    competing_names: BTreeSet<String>,
}

fn add_factor_to_column_scope(
    catalog: &Catalog,
    factor: &ast::TableFactor,
    table_id: TableId,
    column_name: &str,
    scope: &mut ColumnReferenceScope,
) {
    let Ok(bound) = scope::bind_table_factor_scope(catalog, factor) else {
        return;
    };
    for column in bound.columns.iter().filter(|column| column.depth == 0) {
        if !column.qualifier.is_empty() {
            scope.source_qualifiers.insert(column.qualifier.clone());
        }
        if column.table_id == Some(table_id) && column.source_name == column_name {
            scope.target_sources += 1;
            if column.unqualified {
                *scope.target_names.entry(column.name.clone()).or_default() += 1;
            }
            if !column.qualifier.is_empty() {
                scope
                    .target_columns
                    .insert((column.qualifier.clone(), column.name.clone()));
            }
        } else if column.unqualified {
            scope.competing_names.insert(column.name.clone());
        }
    }
}

fn build_column_reference_scope(
    catalog: &Catalog,
    select: &ast::Select,
    table_id: TableId,
    column_name: &str,
    masked: &[String],
) -> ColumnReferenceScope {
    let mut scope = ColumnReferenceScope::default();
    for table in &select.from {
        if !is_masked_table_factor(&table.relation, masked) {
            add_factor_to_column_scope(catalog, &table.relation, table_id, column_name, &mut scope);
        }
        for join in &table.joins {
            if !is_masked_table_factor(&join.relation, masked) {
                add_factor_to_column_scope(
                    catalog,
                    &join.relation,
                    table_id,
                    column_name,
                    &mut scope,
                );
            }
        }
    }
    scope
}

fn is_masked_table_factor(factor: &ast::TableFactor, masked: &[String]) -> bool {
    let ast::TableFactor::Table {
        name, args: None, ..
    } = factor
    else {
        return false;
    };
    normalize_relation_name(name)
        .is_ok_and(|name| name.schema.is_none() && masked.contains(&name.name))
}

fn references_target_column(scopes: &[ColumnReferenceScope], expression: &ast::Expr) -> bool {
    match expression {
        ast::Expr::Identifier(identifier) => {
            let name = normalize_identifier(identifier);
            for scope in scopes.iter().rev() {
                let targets = scope.target_names.get(&name).copied().unwrap_or_default();
                if targets != 0 || scope.competing_names.contains(&name) {
                    return targets == 1 && !scope.competing_names.contains(&name);
                }
            }
            false
        }
        ast::Expr::CompoundIdentifier(identifiers) if identifiers.len() >= 2 => {
            let name = normalize_identifier(&identifiers[identifiers.len() - 1]);
            let qualifier = normalize_identifier(&identifiers[identifiers.len() - 2]);
            for scope in scopes.iter().rev() {
                if scope
                    .target_columns
                    .contains(&(qualifier.clone(), name.clone()))
                {
                    return true;
                }
                if scope.source_qualifiers.contains(&qualifier) {
                    return false;
                }
            }
            false
        }
        _ => false,
    }
}

#[derive(Default)]
struct FactorColumnSummary {
    output_names: BTreeSet<String>,
    target_names: BTreeSet<String>,
    depends_on_target: bool,
}

fn get_join_constraint(operator: &ast::JoinOperator) -> Option<&ast::JoinConstraint> {
    match operator {
        ast::JoinOperator::Join(constraint)
        | ast::JoinOperator::Inner(constraint)
        | ast::JoinOperator::Left(constraint)
        | ast::JoinOperator::LeftOuter(constraint)
        | ast::JoinOperator::Right(constraint)
        | ast::JoinOperator::RightOuter(constraint)
        | ast::JoinOperator::FullOuter(constraint)
        | ast::JoinOperator::CrossJoin(constraint)
        | ast::JoinOperator::Semi(constraint)
        | ast::JoinOperator::LeftSemi(constraint)
        | ast::JoinOperator::RightSemi(constraint)
        | ast::JoinOperator::Anti(constraint)
        | ast::JoinOperator::LeftAnti(constraint)
        | ast::JoinOperator::RightAnti(constraint)
        | ast::JoinOperator::StraightJoin(constraint) => Some(constraint),
        ast::JoinOperator::AsOf { constraint, .. } => Some(constraint),
        ast::JoinOperator::CrossApply
        | ast::JoinOperator::OuterApply
        | ast::JoinOperator::ArrayJoin
        | ast::JoinOperator::LeftArrayJoin
        | ast::JoinOperator::InnerArrayJoin => None,
    }
}

fn apply_column_aliases(names: &mut [String], alias: Option<&ast::TableAlias>) {
    let Some(alias) = alias else {
        return;
    };
    for (name, alias) in names.iter_mut().zip(&alias.columns) {
        *name = normalize_identifier(&alias.name);
    }
}

fn summarize_factor_columns(
    catalog: &Catalog,
    factor: &ast::TableFactor,
    table_id: TableId,
    column_name: &str,
) -> FactorColumnSummary {
    match factor {
        ast::TableFactor::Table {
            name,
            alias,
            args: None,
            ..
        } => {
            let Ok(name) = normalize_relation_name(name) else {
                return FactorColumnSummary::default();
            };
            if let Ok(table) = catalog.require_named_table(&name) {
                let mut names = table
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>();
                let target_index = (table.id == table_id)
                    .then(|| {
                        table
                            .columns
                            .iter()
                            .position(|column| column.name == column_name)
                    })
                    .flatten();
                apply_column_aliases(&mut names, alias.as_ref());
                return FactorColumnSummary {
                    output_names: names.iter().cloned().collect(),
                    target_names: target_index
                        .map(|index| BTreeSet::from([names[index].clone()]))
                        .unwrap_or_default(),
                    depends_on_target: false,
                };
            }
            let Ok(view) = catalog.require_named_view(&name) else {
                return FactorColumnSummary::default();
            };
            let mut names = view
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect::<Vec<_>>();
            apply_column_aliases(&mut names, alias.as_ref());
            FactorColumnSummary {
                output_names: names.into_iter().collect(),
                ..FactorColumnSummary::default()
            }
        }
        ast::TableFactor::Derived {
            subquery, alias, ..
        } => {
            let mut names = infer_query_output_columns(catalog, subquery)
                .unwrap_or_default()
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>();
            apply_column_aliases(&mut names, alias.as_ref());
            FactorColumnSummary {
                output_names: names.into_iter().collect(),
                ..FactorColumnSummary::default()
            }
        }
        ast::TableFactor::NestedJoin {
            table_with_joins, ..
        } => summarize_join_columns(catalog, table_with_joins, table_id, column_name),
        _ => FactorColumnSummary::default(),
    }
}

fn summarize_join_columns(
    catalog: &Catalog,
    table: &ast::TableWithJoins,
    table_id: TableId,
    column_name: &str,
) -> FactorColumnSummary {
    let mut left = summarize_factor_columns(catalog, &table.relation, table_id, column_name);
    for join in &table.joins {
        let right = summarize_factor_columns(catalog, &join.relation, table_id, column_name);
        if let Some(constraint) = get_join_constraint(&join.join_operator) {
            match constraint {
                ast::JoinConstraint::Using(columns) => {
                    left.depends_on_target |= columns.iter().any(|column| {
                        normalize_unqualified_object_name(column).is_ok_and(|name| {
                            left.target_names.contains(&name) || right.target_names.contains(&name)
                        })
                    });
                }
                ast::JoinConstraint::Natural => {
                    left.depends_on_target |= left
                        .output_names
                        .intersection(&right.output_names)
                        .any(|name| {
                            left.target_names.contains(name) || right.target_names.contains(name)
                        });
                }
                ast::JoinConstraint::On(_) | ast::JoinConstraint::None => {}
            }
        }
        left.depends_on_target |= right.depends_on_target;
        left.output_names.extend(right.output_names);
        left.target_names.extend(right.target_names);
    }
    left
}

struct ColumnReferenceDetector<'a> {
    catalog: &'a Catalog,
    table_id: TableId,
    column_name: &'a str,
    scopes: Vec<ColumnReferenceScope>,
    cte_scopes: Vec<CteNameScope>,
    found: bool,
}

impl ast::VisitorMut for ColumnReferenceDetector<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        enter_cte_scope(&mut self.cte_scopes, query);
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.cte_scopes
            .pop()
            .expect("visited query pushed a CTE scope");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &mut ast::Select) -> std::ops::ControlFlow<Self::Break> {
        let masked = self
            .cte_scopes
            .last()
            .map(|scope| scope.body_mask.as_slice())
            .unwrap_or_default();
        let scope = build_column_reference_scope(
            self.catalog,
            select,
            self.table_id,
            self.column_name,
            masked,
        );
        self.found |= select.from.iter().any(|table| {
            !is_masked_table_factor(&table.relation, masked)
                && summarize_join_columns(self.catalog, table, self.table_id, self.column_name)
                    .depends_on_target
        });
        self.found |= select.projection.iter().any(|item| match item {
            ast::SelectItem::Wildcard(_) => scope.target_sources != 0,
            ast::SelectItem::QualifiedWildcard(
                ast::SelectItemQualifiedWildcardKind::ObjectName(name),
                _,
            ) => normalize_unqualified_object_name(name).is_ok_and(|name| {
                scope
                    .target_columns
                    .iter()
                    .any(|(qualifier, _)| qualifier == &name)
            }),
            _ => false,
        });
        self.scopes.push(scope);
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_select(
        &mut self,
        _select: &mut ast::Select,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.scopes
            .pop()
            .expect("visited SELECT pushed a dependency scope");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if references_target_column(&self.scopes, expression) {
            self.found = true;
        }
        std::ops::ControlFlow::Continue(())
    }
}

pub(super) fn collect_view_column_dependencies(
    catalog: &Catalog,
    query: &ast::Query,
    dependencies: &BTreeSet<ViewDependency>,
) -> BTreeMap<TableId, BTreeSet<String>> {
    dependencies
        .iter()
        .filter_map(|dependency| match dependency {
            ViewDependency::Table(table_id) => Some(*table_id),
            _ => None,
        })
        .map(|table_id| {
            let columns = catalog
                .require_table_by_id(table_id)
                .expect("bound view table remains in the catalog")
                .columns
                .iter()
                .filter_map(|column| {
                    let mut query = query.clone();
                    let mut detector = ColumnReferenceDetector {
                        catalog,
                        table_id,
                        column_name: &column.name,
                        scopes: Vec::new(),
                        cte_scopes: Vec::new(),
                        found: false,
                    };
                    let _ = query.visit(&mut detector);
                    detector.found.then_some(column.name.clone())
                })
                .collect();
            (table_id, columns)
        })
        .collect()
}

pub(crate) fn has_view_column_dependency(
    catalog: &Catalog,
    table_id: TableId,
    column_name: &str,
) -> bool {
    catalog.iterate_views().any(|view| {
        view.column_dependencies
            .get(&table_id)
            .is_some_and(|columns| columns.contains(column_name))
    })
}
