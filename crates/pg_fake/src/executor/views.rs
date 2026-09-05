use super::*;
use crate::catalog::{ViewColumn, ViewDependency, ViewId, ViewSchema};
use ast::VisitMut as _;

#[derive(Clone)]
struct CteMaskFrame {
    body_mask: Vec<String>,
    cte_queries: Vec<Box<ast::Query>>,
    cte_masks: Vec<Vec<String>>,
    next_cte: usize,
}

fn enter_cte_scope(stack: &mut Vec<CteMaskFrame>, query: &ast::Query) {
    let inherited = stack.last_mut().map_or_else(Vec::new, |parent| {
        if parent
            .cte_queries
            .get(parent.next_cte)
            .is_some_and(|candidate| candidate.as_ref() == query)
        {
            let mask = parent.cte_masks[parent.next_cte].clone();
            parent.next_cte += 1;
            mask
        } else {
            parent.body_mask.clone()
        }
    });
    let cte_queries = query
        .with
        .as_ref()
        .map(|with| {
            with.cte_tables
                .iter()
                .map(|cte| cte.query.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let names = query
        .with
        .as_ref()
        .map(|with| {
            with.cte_tables
                .iter()
                .map(|cte| normalize_identifier(&cte.alias.name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let recursive = query.with.as_ref().is_some_and(|with| with.recursive);
    let cte_masks = names
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let mut mask = inherited.clone();
            mask.extend(if recursive {
                names.iter().cloned()
            } else {
                names[..index].iter().cloned()
            });
            mask
        })
        .collect();
    let mut body_mask = inherited;
    body_mask.extend(names);
    stack.push(CteMaskFrame {
        body_mask,
        cte_queries,
        cte_masks,
        next_cte: 0,
    });
}

struct ViewDependencyCollector<'a> {
    catalog: &'a Catalog,
    dependencies: BTreeSet<ViewDependency>,
    permanent: bool,
    cte_scopes: Vec<CteMaskFrame>,
    error: Option<PgError>,
}

impl ast::VisitorMut for ViewDependencyCollector<'_> {
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

    fn pre_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        let ast::TableFactor::Table {
            name: object_name,
            alias,
            args: None,
            ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let mut name = match normalize_relation_name(object_name) {
            Ok(name) => name,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        if name.schema.is_none()
            && self
                .cte_scopes
                .last()
                .is_some_and(|scope| scope.body_mask.contains(&name.name))
        {
            return std::ops::ControlFlow::Continue(());
        }
        let implicit_alias = name.name.clone();
        match self.catalog.require_named_table(&name) {
            Ok(table) => {
                if self.permanent && matches!(table.persistence, TablePersistence::Temporary { .. })
                {
                    self.error = Some(PgError::create(
                        SqlState::InvalidTableDefinition,
                        "cannot create a permanent view from a temporary relation",
                    ));
                    return std::ops::ControlFlow::Break(());
                }
                self.dependencies.insert(ViewDependency::Table(table.id));
                name = RelationName::create(
                    Some(self.catalog.get_schema_name(table.schema_id).to_owned()),
                    table.name.clone(),
                );
            }
            Err(error) if error.sqlstate == SqlState::WrongObjectType => {
                match self.catalog.require_named_view(&name) {
                    Ok(view) => {
                        if self.permanent
                            && self.catalog.get_schema_name(view.schema_id) == TEMP_SCHEMA
                        {
                            self.error = Some(PgError::create(
                                SqlState::InvalidTableDefinition,
                                "cannot create a permanent view from a temporary relation",
                            ));
                            return std::ops::ControlFlow::Break(());
                        }
                        self.dependencies.insert(ViewDependency::View(view.id));
                        name = RelationName::create(
                            Some(self.catalog.get_schema_name(view.schema_id).to_owned()),
                            view.name.clone(),
                        );
                    }
                    Err(error) => {
                        self.error = Some(error);
                        return std::ops::ControlFlow::Break(());
                    }
                }
            }
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        *object_name = relation_name_to_object_name(name);
        if alias.is_none() {
            *alias = Some(ast::TableAlias {
                explicit: true,
                name: ast::Ident::with_quote('"', implicit_alias),
                columns: Vec::new(),
                at: None,
            });
        }
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        let ast::Expr::Function(function) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(function_name) = normalize_unqualified_object_name(&function.name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if !matches!(function_name.as_str(), "nextval" | "currval" | "setval") {
            return std::ops::ControlFlow::Continue(());
        }
        let ast::FunctionArguments::List(arguments) = &mut function.args else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))) =
            arguments.args.first_mut()
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(literal) = sequence_literal_mut(argument) else {
            return std::ops::ControlFlow::Continue(());
        };
        let name = match normalize_sequence_name(literal) {
            Ok(name) => name,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        let sequence = match self.catalog.require_named_sequence(&name) {
            Ok(sequence) => sequence,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        if self.permanent && self.catalog.get_schema_name(sequence.schema_id) == TEMP_SCHEMA {
            self.error = Some(PgError::create(
                SqlState::InvalidTableDefinition,
                "cannot create a permanent view from a temporary relation",
            ));
            return std::ops::ControlFlow::Break(());
        }
        self.dependencies
            .insert(ViewDependency::Sequence(sequence.id));
        *literal = format!(
            "{}.{}",
            quote_identifier(self.catalog.get_schema_name(sequence.schema_id)),
            quote_identifier(&sequence.name)
        );
        std::ops::ControlFlow::Continue(())
    }
}

fn sequence_literal_mut(expression: &mut ast::Expr) -> Option<&mut String> {
    match expression {
        ast::Expr::Cast { expr, .. } | ast::Expr::Nested(expr) => sequence_literal_mut(expr),
        ast::Expr::Value(value) => match &mut value.value {
            ast::Value::SingleQuotedString(value) => Some(value),
            _ => None,
        },
        _ => None,
    }
}

fn quote_identifier(identifier: &str) -> String {
    ast::Ident::with_quote('"', identifier).to_string()
}

fn relation_name_to_object_name(name: RelationName) -> ast::ObjectName {
    let mut parts = Vec::with_capacity(2);
    if let Some(schema) = name.schema {
        parts.push(ast::ObjectNamePart::Identifier(ast::Ident::with_quote(
            '"', schema,
        )));
    }
    parts.push(ast::ObjectNamePart::Identifier(ast::Ident::with_quote(
        '"', name.name,
    )));
    ast::ObjectName(parts)
}

fn freeze_view_output(query: &ast::Query, columns: &[ViewColumn]) -> Result<Box<ast::Query>> {
    let alias = quote_identifier("__pg_fake_view_input");
    let names = columns
        .iter()
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>();
    let projection = names
        .iter()
        .map(|name| format!("{alias}.{name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let aliases = names.join(", ");
    let sql = format!("SELECT {projection} FROM ({query}) AS {alias} ({aliases})");
    let mut statements = crate::parser::parse(&sql)?;
    let ast::Statement::Query(query) = statements
        .pop()
        .expect("generated view projection contains one statement")
    else {
        unreachable!("generated view projection is a query")
    };
    Ok(query)
}

struct ViewExpander<'a> {
    catalog: &'a Catalog,
    stack: Vec<ViewId>,
    masked: Vec<Vec<String>>,
    error: Option<PgError>,
}

struct TableReferenceRenamer<'a> {
    catalog: &'a Catalog,
    table_id: TableId,
    new_name: &'a str,
    cte_scopes: Vec<CteMaskFrame>,
}

impl ast::VisitorMut for TableReferenceRenamer<'_> {
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

    fn pre_visit_relation(
        &mut self,
        relation: &mut ast::ObjectName,
    ) -> std::ops::ControlFlow<Self::Break> {
        let Ok(name) = normalize_relation_name(relation) else {
            return std::ops::ControlFlow::Continue(());
        };
        if name.schema.is_none()
            && self
                .cte_scopes
                .last()
                .is_some_and(|scope| scope.body_mask.contains(&name.name))
        {
            return std::ops::ControlFlow::Continue(());
        }
        if self
            .catalog
            .require_named_table(&name)
            .is_ok_and(|table| table.id == self.table_id)
        {
            let ast::ObjectNamePart::Identifier(identifier) = relation
                .0
                .last_mut()
                .expect("normalized relation name is non-empty")
            else {
                unreachable!("normalized relation name ends in an identifier")
            };
            identifier.value = self.new_name.to_owned();
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[derive(Default)]
struct ColumnRenameScope {
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
    scope: &mut ColumnRenameScope,
) {
    let Ok(bound) = super::scope::bind_table_factor_scope(catalog, factor) else {
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

fn build_column_rename_scope(
    catalog: &Catalog,
    select: &ast::Select,
    table_id: TableId,
    column_name: &str,
    masked: &[String],
) -> ColumnRenameScope {
    let mut scope = ColumnRenameScope::default();
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

fn expression_targets_column(scopes: &[ColumnRenameScope], expression: &ast::Expr) -> bool {
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
    scopes: Vec<ColumnRenameScope>,
    cte_scopes: Vec<CteMaskFrame>,
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
        let scope = build_column_rename_scope(
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
        if expression_targets_column(&self.scopes, expression) {
            self.found = true;
        }
        std::ops::ControlFlow::Continue(())
    }
}

fn collect_view_column_dependencies(
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

struct ColumnSourceWrapper<'a> {
    catalog: &'a Catalog,
    table: &'a TableSchema,
    old_name: &'a str,
    new_name: Option<&'a str>,
    dependent_columns: &'a BTreeSet<String>,
    preserve_full_arity: bool,
    skip_generated_source: bool,
    cte_scopes: Vec<CteMaskFrame>,
    nested_join_depth: usize,
}

impl ColumnSourceWrapper<'_> {
    fn get_bound_name<'a>(&'a self, column: &'a ColumnDef) -> &'a str {
        if self.new_name == Some(column.name.as_str()) {
            self.old_name
        } else {
            &column.name
        }
    }
}

impl ast::VisitorMut for ColumnSourceWrapper<'_> {
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

    fn pre_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        if matches!(factor, ast::TableFactor::NestedJoin { .. }) {
            self.nested_join_depth += 1;
            return std::ops::ControlFlow::Continue(());
        }
        if self.skip_generated_source {
            self.skip_generated_source = false;
            return std::ops::ControlFlow::Continue(());
        }
        let ast::TableFactor::Table {
            name,
            alias,
            args: None,
            ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(relation_name) = normalize_relation_name(name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if relation_name.schema.is_none()
            && self
                .cte_scopes
                .last()
                .is_some_and(|scope| scope.body_mask.contains(&relation_name.name))
        {
            return std::ops::ControlFlow::Continue(());
        }
        let matches_table = self
            .catalog
            .require_named_table(&relation_name)
            .is_ok_and(|table| table.id == self.table.id)
            || (relation_name.name == self.table.name
                && relation_name.schema.as_deref().is_none_or(|schema| {
                    schema == self.catalog.get_schema_name(self.table.schema_id)
                }));
        if !matches_table {
            return std::ops::ControlFlow::Continue(());
        }
        let source_alias = quote_identifier("__pg_fake_column_source");
        let dependency_len = self
            .table
            .columns
            .iter()
            .rposition(|column| self.dependent_columns.contains(self.get_bound_name(column)))
            .map_or(0, |index| index + 1);
        let projection_len = if self.preserve_full_arity || self.nested_join_depth != 0 {
            self.table.columns.len()
        } else {
            dependency_len.max(alias.as_ref().map_or(0, |alias| alias.columns.len()))
        };
        assert_ne!(projection_len, 0);
        let projections = self
            .table
            .columns
            .iter()
            .take(projection_len)
            .map(|column| {
                let bound_name = self.get_bound_name(column);
                if !self.dependent_columns.contains(bound_name) {
                    return format!("NULL AS {}", quote_identifier(bound_name));
                }
                let source = quote_identifier(&column.name);
                let output = quote_identifier(bound_name);
                if bound_name == column.name {
                    format!("{source_alias}.{source}")
                } else {
                    format!("{source_alias}.{source} AS {output}")
                }
            })
            .collect::<Vec<_>>();
        let sql = format!(
            "SELECT {} FROM {} AS {source_alias}",
            projections.join(", "),
            relation_name_to_object_name(relation_name)
        );
        let mut statements = crate::parser::parse(&sql).expect("generated column wrapper parses");
        let ast::Statement::Query(query) = statements
            .pop()
            .expect("generated column wrapper contains one statement")
        else {
            unreachable!("generated column wrapper is a query")
        };
        *factor = ast::TableFactor::Derived {
            lateral: false,
            subquery: query,
            alias: alias.clone(),
            sample: None,
        };
        self.skip_generated_source = true;
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        if matches!(factor, ast::TableFactor::NestedJoin { .. }) {
            self.nested_join_depth -= 1;
        }
        std::ops::ControlFlow::Continue(())
    }
}

impl ast::VisitorMut for ViewExpander<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.masked.push(
            query
                .with
                .as_ref()
                .map(|with| {
                    with.cte_tables
                        .iter()
                        .map(|cte| normalize_identifier(&cte.alias.name))
                        .collect()
                })
                .unwrap_or_default(),
        );
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.masked.pop().expect("visited query pushed a CTE mask");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        let ast::TableFactor::Table {
            name,
            alias,
            args: None,
            ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let relation_name = match normalize_relation_name(name) {
            Ok(name) => name,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        if relation_name.schema.is_none()
            && self
                .masked
                .iter()
                .any(|names| names.contains(&relation_name.name))
        {
            return std::ops::ControlFlow::Continue(());
        }
        let view = match self.catalog.require_named_view(&relation_name) {
            Ok(view) => view,
            Err(error)
                if matches!(
                    error.sqlstate,
                    SqlState::UndefinedTable | SqlState::WrongObjectType
                ) =>
            {
                return std::ops::ControlFlow::Continue(());
            }
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        if self.stack.contains(&view.id) {
            self.error = Some(PgError::create(
                SqlState::InvalidObjectDefinition,
                format!(
                    "infinite recursion detected in rules for relation {:?}",
                    view.name
                ),
            ));
            return std::ops::ControlFlow::Break(());
        }
        let mut query = view.query.as_ref().clone();
        let mut expander = ViewExpander {
            catalog: self.catalog,
            stack: self
                .stack
                .iter()
                .copied()
                .chain(std::iter::once(view.id))
                .collect(),
            masked: Vec::new(),
            error: None,
        };
        let _ = query.visit(&mut expander);
        if let Some(error) = expander.error {
            self.error = Some(error);
            return std::ops::ControlFlow::Break(());
        }
        let query = match freeze_view_output(&query, &view.columns) {
            Ok(query) => query,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        let columns = view
            .columns
            .iter()
            .map(|column| ast::TableAliasColumnDef {
                name: ast::Ident::with_quote('"', column.name.clone()),
                data_type: None,
            })
            .collect::<Vec<_>>();
        let alias = match alias {
            Some(alias) if alias.columns.is_empty() => ast::TableAlias {
                columns,
                ..alias.clone()
            },
            Some(alias) => alias.clone(),
            None => ast::TableAlias {
                explicit: true,
                name: ast::Ident::with_quote('"', view.name.clone()),
                columns,
                at: None,
            },
        };
        *factor = ast::TableFactor::Derived {
            lateral: false,
            subquery: query,
            alias: Some(alias),
            sample: None,
        };
        std::ops::ControlFlow::Continue(())
    }
}

pub(crate) fn expand_query_views(catalog: &Catalog, query: &ast::Query) -> Result<ast::Query> {
    let mut query = query.clone();
    let mut expander = ViewExpander {
        catalog,
        stack: Vec::new(),
        masked: Vec::new(),
        error: None,
    };
    let _ = query.visit(&mut expander);
    expander.error.map_or(Ok(query), Err)
}

pub(crate) fn rename_table_references(catalog: &mut Catalog, table_id: TableId, new_name: &str) {
    let snapshot = catalog.clone();
    for view in catalog
        .iterate_views_mut()
        .filter(|view| view.dependencies.contains(&ViewDependency::Table(table_id)))
    {
        let mut query = view.query.as_ref().clone();
        let mut renamer = TableReferenceRenamer {
            catalog: &snapshot,
            table_id,
            new_name,
            cte_scopes: Vec::new(),
        };
        let _ = query.visit(&mut renamer);
        view.query = Box::new(query);
    }
}

pub(crate) fn rename_column_references(
    catalog: &mut Catalog,
    table: &TableSchema,
    old_name: &str,
    new_name: &str,
) {
    let table_id = table.id;
    let snapshot = catalog.clone();
    for view in catalog
        .iterate_views_mut()
        .filter(|view| view.dependencies.contains(&ViewDependency::Table(table_id)))
    {
        let Some(columns) = view.column_dependencies.get_mut(&table_id) else {
            continue;
        };
        if !columns.contains(old_name) {
            continue;
        }
        let mut query = view.query.as_ref().clone();
        let mut wrapper = ColumnSourceWrapper {
            catalog: &snapshot,
            table,
            old_name,
            new_name: Some(new_name),
            dependent_columns: columns,
            preserve_full_arity: false,
            skip_generated_source: false,
            cte_scopes: Vec::new(),
            nested_join_depth: 0,
        };
        let _ = query.visit(&mut wrapper);
        assert!(columns.remove(old_name));
        columns.insert(new_name.to_owned());
        view.query = Box::new(query);
    }
}

pub(crate) fn preserve_column_drop_references(
    catalog: &mut Catalog,
    table: &TableSchema,
    column_name: &str,
) {
    let table_id = table.id;
    let snapshot = catalog.clone();
    for view in catalog
        .iterate_views_mut()
        .filter(|view| view.dependencies.contains(&ViewDependency::Table(table_id)))
    {
        let columns = view
            .column_dependencies
            .get(&table_id)
            .expect("table view dependency has column dependency storage");
        assert!(!columns.contains(column_name));
        let mut query = view.query.as_ref().clone();
        let mut wrapper = ColumnSourceWrapper {
            catalog: &snapshot,
            table,
            old_name: column_name,
            new_name: None,
            dependent_columns: columns,
            preserve_full_arity: true,
            skip_generated_source: false,
            cte_scopes: Vec::new(),
            nested_join_depth: 0,
        };
        let _ = query.visit(&mut wrapper);
        view.query = Box::new(query);
    }
}

fn bind_view_dependencies(
    state: &DatabaseState,
    query: &ast::Query,
    permanent: bool,
) -> Result<(Box<ast::Query>, BTreeSet<ViewDependency>)> {
    let mut query = query.clone();
    let mut collector = ViewDependencyCollector {
        catalog: &state.catalog,
        dependencies: BTreeSet::new(),
        permanent,
        cte_scopes: Vec::new(),
        error: None,
    };
    let _ = query.visit(&mut collector);
    match collector.error {
        Some(error) => Err(error),
        None => {
            collector.dependencies.extend(
                super::query::collect_query_primary_key_dependencies(state, &query)
                    .into_iter()
                    .map(ViewDependency::Constraint),
            );
            Ok((Box::new(query), collector.dependencies))
        }
    }
}

fn has_view_dependency_path(catalog: &Catalog, start: ViewId, target: ViewId) -> bool {
    let mut pending = vec![start];
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if id == target {
            return true;
        }
        if !visited.insert(id) {
            continue;
        }
        if let Some(view) = catalog.iterate_views().find(|view| view.id == id) {
            pending.extend(
                view.dependencies
                    .iter()
                    .filter_map(|dependency| match dependency {
                        ViewDependency::View(id) => Some(*id),
                        ViewDependency::Table(_)
                        | ViewDependency::Sequence(_)
                        | ViewDependency::Constraint(_) => None,
                    }),
            );
        }
    }
    false
}

pub(crate) fn execute_create_view(
    state: &mut DatabaseState,
    create: &ast::CreateView,
) -> Result<StatementResult> {
    if create.or_alter
        || create.materialized
        || create.secure
        || create.if_not_exists
        || create.name_before_not_exists
        || !matches!(create.options, ast::CreateTableOptions::None)
        || !create.cluster_by.is_empty()
        || create.comment.is_some()
        || create.with_no_schema_binding
        || create.copy_grants
        || create.to.is_some()
        || create.params.is_some()
    {
        return reject_unsupported("CREATE VIEW variant is not implemented");
    }
    if create
        .columns
        .iter()
        .any(|column| column.data_type.is_some() || column.options.is_some())
    {
        return reject_unsupported("CREATE VIEW column options are not implemented");
    }
    if crate::analyzer::count_parameters(&ast::Statement::Query(create.query.clone()))? != 0 {
        return Err(PgError::create(
            SqlState::UndefinedParameter,
            "there is no parameter in CREATE VIEW",
        ));
    }
    let name = normalize_relation_name(&create.name)?;
    let temporary = create.temporary || name.schema.as_deref() == Some(TEMP_SCHEMA);
    let resolved = state.catalog.resolve_creation_name(&name, temporary)?;
    let resolved_name = RelationName::create(
        Some(state.catalog.get_schema_name(resolved.schema_id).to_owned()),
        resolved.name.clone(),
    );
    let existing = state
        .catalog
        .require_named_view(&resolved_name)
        .ok()
        .cloned();
    if !create.or_replace && state.catalog.has_resolved_relation(&resolved) {
        return Err(PgError::create(
            SqlState::DuplicateTable,
            format!("relation {:?} already exists", resolved.name),
        ));
    }
    if create.or_replace && existing.is_none() && state.catalog.has_resolved_relation(&resolved) {
        return Err(PgError::create(
            SqlState::WrongObjectType,
            format!("{:?} is not a view", resolved.name),
        ));
    }
    let (expanded, mutations) =
        super::expand_ctes_for_analysis(&ast::Statement::Query(create.query.clone()), state)?;
    if !mutations.is_empty() {
        return Err(PgError::create(
            SqlState::FeatureNotSupported,
            "views cannot contain data-modifying statements",
        ));
    }
    let ast::Statement::Query(expanded) = expanded else {
        unreachable!("view definition is a query")
    };
    let inferred = infer_query_output_columns(&state.catalog, &expanded)?;
    if create.columns.len() > inferred.len() {
        return Err(PgError::create(
            SqlState::InvalidTableDefinition,
            "CREATE VIEW specifies more column names than columns",
        ));
    }
    let columns = inferred
        .into_iter()
        .enumerate()
        .map(|(index, (name, data_type))| ViewColumn {
            name: create
                .columns
                .get(index)
                .map(|column| normalize_identifier(&column.name))
                .unwrap_or(name),
            data_type,
        })
        .collect::<Vec<_>>();
    let mut names = BTreeSet::new();
    if columns
        .iter()
        .any(|column| !names.insert(column.name.clone()))
    {
        return Err(PgError::create(
            SqlState::DuplicateColumn,
            "column name specified more than once",
        ));
    }
    let (query, dependencies) = bind_view_dependencies(state, &create.query, !temporary)?;
    let column_dependencies =
        collect_view_column_dependencies(&state.catalog, &query, &dependencies);
    if let Some(existing) = existing {
        if columns.len() < existing.columns.len()
            || existing
                .columns
                .iter()
                .zip(&columns)
                .any(|(old, new)| old.name != new.name || old.data_type != new.data_type)
        {
            return Err(PgError::create(
                SqlState::InvalidTableDefinition,
                "cannot change name or data type of view column",
            ));
        }
        if dependencies.iter().any(|dependency| {
            matches!(dependency, ViewDependency::View(id) if has_view_dependency_path(&state.catalog, *id, existing.id))
        }) {
            return Err(PgError::create(
                SqlState::InvalidObjectDefinition,
                "infinite recursion detected in rules for relation",
            ));
        }
        state.catalog.replace_view(ViewSchema {
            id: existing.id,
            schema_id: existing.schema_id,
            name: existing.name,
            columns,
            query,
            comment: existing.comment,
            dependencies,
            column_dependencies,
        })?;
    } else {
        state.catalog.create_named_view(
            resolved,
            columns,
            query,
            dependencies,
            column_dependencies,
        )?;
    }
    Ok(StatementResult::Affected(0))
}

pub(crate) fn execute_drop_views(
    state: &mut DatabaseState,
    names: &[ast::ObjectName],
    if_exists: bool,
    cascade: bool,
) -> Result<StatementResult> {
    if cascade {
        return reject_unsupported("DROP VIEW CASCADE is not implemented");
    }
    let mut views = Vec::new();
    let mut seen = BTreeSet::new();
    for object in names {
        let name = normalize_relation_name(object)?;
        match state.catalog.require_named_view(&name) {
            Ok(view) if seen.insert(view.id) => views.push(name),
            Ok(_) => {}
            Err(error) if if_exists && error.sqlstate == SqlState::UndefinedTable => {}
            Err(error) => return Err(error),
        }
    }
    state.catalog.drop_named_views(&views)?;
    Ok(StatementResult::Affected(0))
}

pub(crate) fn execute_comment_on_view(
    state: &mut DatabaseState,
    name: &ast::ObjectName,
    comment: &Option<String>,
) -> Result<StatementResult> {
    let name = normalize_relation_name(name)?;
    let mut view = state.catalog.require_named_view(&name)?.clone();
    view.comment = comment.clone();
    state.catalog.replace_view(view)?;
    Ok(StatementResult::Affected(0))
}

pub(crate) fn execute_alter_trigger(
    state: &mut DatabaseState,
    name: &ast::Ident,
    table_name: &ast::ObjectName,
    new_name: &ast::Ident,
) -> Result<StatementResult> {
    let name = normalize_identifier(name);
    let new_name = normalize_identifier(new_name);
    let table_name = normalize_relation_name(table_name)?;
    let mut table = state.catalog.require_named_table(&table_name)?.clone();
    if table
        .triggers
        .iter()
        .any(|trigger| trigger.name == new_name)
    {
        return Err(PgError::create(
            SqlState::DuplicateObject,
            format!(
                "trigger {new_name:?} for relation {:?} already exists",
                table.name
            ),
        ));
    }
    let trigger = table
        .triggers
        .iter_mut()
        .find(|trigger| trigger.name == name)
        .ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedObject,
                format!("trigger {name:?} for table {:?} does not exist", table.name),
            )
        })?;
    trigger.name = new_name;
    trigger.definition.name =
        relation_name_to_object_name(RelationName::create_unqualified(trigger.name.clone()));
    table
        .triggers
        .sort_by(|left, right| left.name.cmp(&right.name));
    state.catalog.replace_table(table)?;
    Ok(StatementResult::Affected(0))
}
