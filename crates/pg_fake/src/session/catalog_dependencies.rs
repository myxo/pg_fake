use sqlparser::ast::{self, Visit as _};

use crate::{
    catalog::{
        ConstraintId, RelationName, ResolvedRelationName, SequenceSchema, TableId, TableSchema,
        ViewSchema,
    },
    error::{PgError, Result, SqlState},
    executor,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CatalogDependency {
    Table {
        name: RelationName,
        schema: TableSchema,
    },
    Sequence {
        name: RelationName,
        schema: SequenceSchema,
    },
    Constraint {
        table: TableId,
        id: ConstraintId,
    },
    View {
        name: RelationName,
        schema: ViewSchema,
    },
}

pub(super) fn extract_sequence_name(expression: &ast::Expr) -> Option<&str> {
    match expression {
        ast::Expr::Cast { expr, .. } | ast::Expr::Nested(expr) => extract_sequence_name(expr),
        ast::Expr::Value(value) => match &value.value {
            ast::Value::SingleQuotedString(value) => Some(value),
            _ => None,
        },
        _ => None,
    }
}

struct CatalogDependencyCollector<'catalog> {
    skip_function_name: bool,
    catalog: &'catalog crate::catalog::Catalog,
    dependencies: Vec<CatalogDependency>,
    cte_scopes: Vec<CteScope>,
    error: Option<PgError>,
}

#[derive(Clone)]
struct CteScope {
    body_mask: Vec<String>,
    cte_queries: Vec<ast::Query>,
    cte_masks: Vec<Vec<String>>,
    next_cte: usize,
}

fn enter_cte_scope(stack: &mut Vec<CteScope>, query: &ast::Query) {
    let inherited = stack.last_mut().map_or_else(Vec::new, |parent| {
        if parent
            .cte_queries
            .get(parent.next_cte)
            .is_some_and(|candidate| candidate == query)
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
                .map(|cte| cte.query.as_ref().clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let names = query
        .with
        .as_ref()
        .map(|with| {
            with.cte_tables
                .iter()
                .map(|cte| executor::normalize_identifier(&cte.alias.name))
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
    stack.push(CteScope {
        body_mask,
        cte_queries,
        cte_masks,
        next_cte: 0,
    });
}

impl CatalogDependencyCollector<'_> {
    fn add_dependency(&mut self, dependency: CatalogDependency) {
        if !self.dependencies.contains(&dependency) {
            self.dependencies.push(dependency);
        }
    }

    fn collect_relation(&mut self, relation: &ast::ObjectName) -> Result<()> {
        let name = executor::normalize_relation_name(relation)?;
        let table = match self.catalog.require_named_table(&name) {
            Ok(table) => table.clone(),
            Err(error) if error.sqlstate == SqlState::WrongObjectType => {
                let view = self.catalog.require_named_view(&name)?.clone();
                self.add_dependency(CatalogDependency::View {
                    name,
                    schema: view.clone(),
                });
                let _ = view.query.visit(self);
                if let Some(error) = self.error.take() {
                    return Err(error);
                }
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        self.add_dependency(CatalogDependency::Table {
            name,
            schema: table.clone(),
        });
        for dependency in table
            .constraints
            .iter()
            .map(|constraint| CatalogDependency::Constraint {
                table: table.id,
                id: constraint.get_id(),
            })
        {
            self.add_dependency(dependency);
        }
        for sequence_name in table
            .columns
            .iter()
            .filter_map(|column| column.default_sequence.as_ref())
        {
            let name = RelationName::create(
                Some(
                    self.catalog
                        .get_schema_name(sequence_name.schema_id)
                        .to_owned(),
                ),
                sequence_name.name.clone(),
            );
            let sequence = self.catalog.require_named_sequence(&name)?.clone();
            self.add_dependency(CatalogDependency::Sequence {
                name,
                schema: sequence,
            });
        }
        Ok(())
    }
}

impl ast::Visitor for CatalogDependencyCollector<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &ast::Query) -> std::ops::ControlFlow<Self::Break> {
        enter_cte_scope(&mut self.cte_scopes, query);
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.cte_scopes
            .pop()
            .expect("visited query pushed a CTE scope");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(
        &mut self,
        factor: &ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        if matches!(factor, ast::TableFactor::Table { args: Some(_), .. }) {
            self.skip_function_name = true;
        }
        std::ops::ControlFlow::Continue(())
    }
    fn pre_visit_relation(
        &mut self,
        relation: &ast::ObjectName,
    ) -> std::ops::ControlFlow<Self::Break> {
        if std::mem::take(&mut self.skip_function_name) {
            return std::ops::ControlFlow::Continue(());
        }
        if executor::normalize_relation_name(relation).is_ok_and(|name| {
            name.schema.is_none()
                && self
                    .cte_scopes
                    .last()
                    .is_some_and(|scope| scope.body_mask.contains(&name.name))
        }) {
            return std::ops::ControlFlow::Continue(());
        }
        if let Err(error) = self.collect_relation(relation) {
            self.error = Some(error);
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        let ast::Expr::Function(function) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(name) = executor::normalize_unqualified_object_name(&function.name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if !matches!(name.as_str(), "nextval" | "currval" | "setval") {
            return std::ops::ControlFlow::Continue(());
        }
        let ast::FunctionArguments::List(arguments) = &function.args else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))) =
            arguments.args.first()
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(name) = extract_sequence_name(argument) else {
            return std::ops::ControlFlow::Continue(());
        };
        let name = match executor::normalize_sequence_name(name) {
            Ok(name) => name,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        match self.catalog.require_named_sequence(&name) {
            Ok(sequence) => {
                self.add_dependency(CatalogDependency::Sequence {
                    name,
                    schema: sequence.clone(),
                });
                std::ops::ControlFlow::Continue(())
            }
            Err(error) => {
                self.error = Some(error);
                std::ops::ControlFlow::Break(())
            }
        }
    }
}

pub(super) fn collect_catalog_dependencies<'a>(
    catalog: &crate::catalog::Catalog,
    statements: impl IntoIterator<Item = &'a ast::Statement>,
) -> Result<Vec<CatalogDependency>> {
    let mut collector = CatalogDependencyCollector {
        skip_function_name: false,
        catalog,
        dependencies: Vec::new(),
        cte_scopes: Vec::new(),
        error: None,
    };
    for statement in statements {
        match statement {
            ast::Statement::Query(_)
            | ast::Statement::Insert(_)
            | ast::Statement::Update(_)
            | ast::Statement::Delete(_) => {
                let _ = statement.visit(&mut collector);
            }
            ast::Statement::Drop {
                object_type: ast::ObjectType::Table,
                names,
                ..
            } => {
                for name in names {
                    let Ok(name) = executor::normalize_relation_name(name) else {
                        continue;
                    };
                    if let Ok(table) = catalog.require_named_table(&name) {
                        collector.add_dependency(CatalogDependency::Table {
                            name,
                            schema: table.clone(),
                        });
                    }
                }
            }
            ast::Statement::Drop {
                object_type: ast::ObjectType::Sequence,
                names,
                ..
            } => {
                for name in names {
                    let Ok(name) = executor::normalize_relation_name(name) else {
                        continue;
                    };
                    if let Ok(sequence) = catalog.require_named_sequence(&name) {
                        collector.add_dependency(CatalogDependency::Sequence {
                            name,
                            schema: sequence.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
        if let Some(error) = collector.error.take() {
            return Err(error);
        }
        if let ast::Statement::Insert(insert) = statement
            && let Some(ast::OnInsert::OnConflict(ast::OnConflict {
                conflict_target: Some(ast::ConflictTarget::OnConstraint(name)),
                ..
            })) = &insert.on
        {
            let table_name = executor::resolve_insert_table_name(&insert.table)?;
            let table = catalog.require_named_table(&table_name)?;
            let constraint_name = executor::normalize_unqualified_object_name(name)?;
            let constraint = table
                .constraints
                .iter()
                .find(|constraint| constraint.get_name() == Some(constraint_name.as_str()))
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::UndefinedObject,
                        format!(
                            "constraint {constraint_name:?} for table {:?} does not exist",
                            table.name
                        ),
                    )
                })?;
            collector.add_dependency(CatalogDependency::Constraint {
                table: table.id,
                id: constraint.get_id(),
            });
        }
    }
    Ok(collector.dependencies)
}

fn does_prepared_table_match(current: &TableSchema, prepared: &TableSchema) -> bool {
    current.id == prepared.id
        && current.schema_id == prepared.schema_id
        && current.name == prepared.name
        && current.columns == prepared.columns
        && current.constraints == prepared.constraints
        && current.indexes == prepared.indexes
        && current.persistence == prepared.persistence
}

pub(super) fn validate_catalog_dependencies(
    catalog: &crate::catalog::Catalog,
    dependencies: &[CatalogDependency],
) -> Result<()> {
    let error = dependencies.iter().find_map(|dependency| match dependency {
        CatalogDependency::Table { name, schema } => {
            match catalog.require_named_table(name) {
                Ok(table) if table.id == schema.id => {}
                Ok(_) => {
                    return Some(PgError::create(
                        SqlState::FeatureNotSupported,
                        "cached plan must be replanned",
                    ));
                }
                Err(error) => return Some(error),
            }
            match catalog.require_table_by_id(schema.id) {
                Ok(table) if does_prepared_table_match(table, schema) => None,
                Ok(_) => Some(PgError::create(
                    SqlState::FeatureNotSupported,
                    "cached plan must be replanned",
                )),
                Err(error) => {
                    let name = ResolvedRelationName {
                        schema_id: schema.schema_id,
                        name: schema.name.clone(),
                    };
                    if catalog.has_resolved_relation(&name) {
                        Some(PgError::create(
                            SqlState::FeatureNotSupported,
                            "cached plan must be replanned",
                        ))
                    } else {
                        Some(error)
                    }
                }
            }
        }
        CatalogDependency::Sequence { name, schema } => {
            match catalog.require_named_sequence(name) {
                Ok(sequence) if sequence.id == schema.id => {}
                Ok(_) => {
                    return Some(PgError::create(
                        SqlState::FeatureNotSupported,
                        "cached plan must be replanned",
                    ));
                }
                Err(error) => return Some(error),
            }
            match catalog
                .iterate_sequences()
                .find(|sequence| sequence.id == schema.id)
            {
                Some(sequence) if sequence == schema => None,
                Some(_) => Some(PgError::create(
                    SqlState::FeatureNotSupported,
                    "cached plan must be replanned",
                )),
                None => {
                    let name = ResolvedRelationName {
                        schema_id: schema.schema_id,
                        name: schema.name.clone(),
                    };
                    if catalog.has_resolved_relation(&name) {
                        Some(PgError::create(
                            SqlState::FeatureNotSupported,
                            "cached plan must be replanned",
                        ))
                    } else {
                        Some(PgError::create(
                            SqlState::UndefinedTable,
                            format!("relation {:?} does not exist", schema.name),
                        ))
                    }
                }
            }
        }
        CatalogDependency::Constraint { table, id } => {
            (!catalog.has_constraint(*table, *id)).then(|| {
                PgError::create(
                    SqlState::FeatureNotSupported,
                    "cached plan must be replanned",
                )
            })
        }
        CatalogDependency::View { name, schema } => match catalog.require_named_view(name) {
            Ok(view)
                if view.id == schema.id
                    && view.schema_id == schema.schema_id
                    && view.name == schema.name
                    && view.columns == schema.columns
                    && view.query == schema.query
                    && view.dependencies == schema.dependencies
                    && view.column_dependencies == schema.column_dependencies =>
            {
                None
            }
            Ok(_) => Some(PgError::create(
                SqlState::FeatureNotSupported,
                "cached plan must be replanned",
            )),
            Err(error) => Some(error),
        },
    });
    match error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
