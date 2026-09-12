use crate::catalog::ViewDependency;

use super::Db;

/// A read-only, OID-free description of the committed database catalog.
///
/// This is intended for differential conformance tests and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogInspection {
    pub tables: Vec<CatalogTableInspection>,
    pub sequences: Vec<CatalogSequenceInspection>,
    pub views: Vec<CatalogViewInspection>,
    pub functions: Vec<CatalogFunctionInspection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTableInspection {
    pub schema: String,
    pub name: String,
    pub columns: Vec<CatalogColumnInspection>,
    pub constraints: Vec<CatalogConstraintInspection>,
    pub indexes: Vec<CatalogIndexInspection>,
    pub triggers: Vec<CatalogTriggerInspection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogColumnInspection {
    pub name: String,
    pub type_name: String,
    pub typmod: i32,
    pub default: Option<String>,
    pub nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogConstraintInspection {
    pub name: String,
    pub kind: String,
    pub columns: Vec<String>,
    pub referenced_relation: Option<String>,
    pub referenced_columns: Vec<String>,
    pub on_update: Option<String>,
    pub on_delete: Option<String>,
    pub validated: bool,
    pub predicate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogIndexInspection {
    pub name: String,
    pub unique: bool,
    pub keys: Vec<(String, bool)>,
    pub included_columns: Vec<String>,
    pub predicate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTriggerInspection {
    pub name: String,
    pub timing: String,
    pub events: Vec<String>,
    pub level: String,
    pub function: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSequenceInspection {
    pub schema: String,
    pub name: String,
    pub type_name: String,
    pub increment: i64,
    pub minimum: i64,
    pub maximum: i64,
    pub start: i64,
    pub cycle: bool,
    pub cache: i64,
    pub owner: Option<(String, String)>,
    pub last_value: i64,
    pub is_called: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogViewInspection {
    pub schema: String,
    pub name: String,
    pub columns: Vec<(String, String, i32)>,
    pub definition: String,
    pub comment: Option<String>,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogFunctionInspection {
    pub schema: String,
    pub name: String,
    pub argument_count: usize,
    pub return_type: Option<String>,
    pub language: Option<String>,
}

impl Db {
    /// Inspect the committed catalog without exposing internal object identifiers.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn inspect_catalog(&self) -> CatalogInspection {
        let state = self.state.lock().expect("database mutex is poisoned");
        let catalog = &state.catalog;
        let sequence_values = state
            .sequence_values
            .lock()
            .expect("sequence storage is poisoned");

        let mut tables = catalog
            .iterate_tables()
            .map(|table| {
                let schema = catalog.get_schema_name(table.schema_id).to_owned();
                let mut constraints = table
                    .constraints
                    .iter()
                    .map(|constraint| match constraint {
                        crate::catalog::Constraint::PrimaryKey { name, columns, .. } => {
                            CatalogConstraintInspection {
                                name: name.clone(),
                                kind: "PRIMARY KEY".into(),
                                columns: columns.clone(),
                                referenced_relation: None,
                                referenced_columns: Vec::new(),
                                on_update: None,
                                on_delete: None,
                                validated: true,
                                predicate: None,
                            }
                        }
                        crate::catalog::Constraint::Unique { name, columns, .. } => {
                            CatalogConstraintInspection {
                                name: name.clone(),
                                kind: "UNIQUE".into(),
                                columns: columns.clone(),
                                referenced_relation: None,
                                referenced_columns: Vec::new(),
                                on_update: None,
                                on_delete: None,
                                validated: true,
                                predicate: None,
                            }
                        }
                        crate::catalog::Constraint::Check {
                            name,
                            expression,
                            validated,
                            ..
                        } => CatalogConstraintInspection {
                            name: name.clone(),
                            kind: "CHECK".into(),
                            columns: Vec::new(),
                            referenced_relation: None,
                            referenced_columns: Vec::new(),
                            on_update: None,
                            on_delete: None,
                            validated: *validated,
                            predicate: Some(expression.to_string()),
                        },
                        crate::catalog::Constraint::ForeignKey(foreign_key) => {
                            let foreign_table = catalog
                                .require_table_by_id(foreign_key.foreign_table_id)
                                .expect("foreign key target must remain in the catalog");
                            CatalogConstraintInspection {
                                name: foreign_key.name.clone(),
                                kind: "FOREIGN KEY".into(),
                                columns: foreign_key.columns.clone(),
                                referenced_relation: Some(format!(
                                    "{}.{}",
                                    catalog.get_schema_name(foreign_table.schema_id),
                                    foreign_table.name
                                )),
                                referenced_columns: foreign_key.referred_columns.clone(),
                                on_update: Some(format!("{:?}", foreign_key.on_update)),
                                on_delete: Some(format!("{:?}", foreign_key.on_delete)),
                                validated: foreign_key.validated,
                                predicate: None,
                            }
                        }
                    })
                    .collect::<Vec<_>>();
                constraints.sort_by(|left, right| left.name.cmp(&right.name));

                let mut indexes = table
                    .indexes
                    .iter()
                    .map(|index| CatalogIndexInspection {
                        name: index.name.clone(),
                        unique: index.unique,
                        keys: index
                            .columns
                            .iter()
                            .map(|column| (column.name.clone(), column.descending))
                            .collect(),
                        included_columns: index.include.clone(),
                        predicate: index.predicate.as_ref().map(ToString::to_string),
                    })
                    .collect::<Vec<_>>();
                indexes.sort_by(|left, right| left.name.cmp(&right.name));

                let mut triggers = table
                    .triggers
                    .iter()
                    .map(|trigger| {
                        let function = catalog
                            .require_function_by_id(trigger.function_id)
                            .expect("trigger function must remain in the catalog");
                        CatalogTriggerInspection {
                            name: trigger.name.clone(),
                            timing: trigger
                                .definition
                                .period
                                .map_or_else(String::new, |period| period.to_string()),
                            events: trigger
                                .definition
                                .events
                                .iter()
                                .map(ToString::to_string)
                                .collect(),
                            level: trigger
                                .definition
                                .trigger_object
                                .as_ref()
                                .map_or_else(String::new, ToString::to_string),
                            function: format!(
                                "{}.{}",
                                catalog.get_schema_name(function.schema_id),
                                function.name
                            ),
                        }
                    })
                    .collect::<Vec<_>>();
                triggers.sort_by(|left, right| left.name.cmp(&right.name));

                CatalogTableInspection {
                    schema,
                    name: table.name.clone(),
                    columns: table
                        .columns
                        .iter()
                        .map(|column| CatalogColumnInspection {
                            name: column.name.clone(),
                            type_name: column.data_type.base.get_postgres_name().into(),
                            typmod: column.data_type.typmod,
                            default: column.default.as_ref().map(ToString::to_string),
                            nullable: column.nullable,
                        })
                        .collect(),
                    constraints,
                    indexes,
                    triggers,
                }
            })
            .collect::<Vec<_>>();
        tables.sort_by(|left, right| (&left.schema, &left.name).cmp(&(&right.schema, &right.name)));

        let mut sequences = catalog
            .iterate_sequences()
            .map(|sequence| {
                let value = sequence_values
                    .get(&sequence.id)
                    .expect("visible sequence must have value state");
                let owner = sequence.owned_by.as_ref().map(|(table_id, column)| {
                    let table = catalog
                        .require_table_by_id(*table_id)
                        .expect("sequence owner must remain in the catalog");
                    (
                        format!(
                            "{}.{}",
                            catalog.get_schema_name(table.schema_id),
                            table.name
                        ),
                        column.clone(),
                    )
                });
                CatalogSequenceInspection {
                    schema: catalog.get_schema_name(sequence.schema_id).to_owned(),
                    name: sequence.name.clone(),
                    type_name: sequence.data_type.get_postgres_name().into(),
                    increment: sequence.increment,
                    minimum: sequence.min_value,
                    maximum: sequence.max_value,
                    start: sequence.start_value,
                    cycle: sequence.cycle,
                    cache: sequence.cache,
                    owner,
                    last_value: value.last_value,
                    is_called: value.is_called,
                }
            })
            .collect::<Vec<_>>();
        sequences
            .sort_by(|left, right| (&left.schema, &left.name).cmp(&(&right.schema, &right.name)));

        let mut views = catalog
            .iterate_views()
            .map(|view| {
                let mut dependencies = view
                    .dependencies
                    .iter()
                    .map(|dependency| match dependency {
                        ViewDependency::Table(id) => {
                            let table = catalog
                                .require_table_by_id(*id)
                                .expect("view table dependency must remain visible");
                            format!(
                                "table:{}.{}",
                                catalog.get_schema_name(table.schema_id),
                                table.name
                            )
                        }
                        ViewDependency::View(id) => {
                            let dependency = catalog
                                .iterate_views()
                                .find(|candidate| candidate.id == *id)
                                .expect("view dependency must remain visible");
                            format!(
                                "view:{}.{}",
                                catalog.get_schema_name(dependency.schema_id),
                                dependency.name
                            )
                        }
                        ViewDependency::Sequence(id) => {
                            let dependency = catalog
                                .iterate_sequences()
                                .find(|candidate| candidate.id == *id)
                                .expect("view sequence dependency must remain visible");
                            format!(
                                "sequence:{}.{}",
                                catalog.get_schema_name(dependency.schema_id),
                                dependency.name
                            )
                        }
                        ViewDependency::Constraint(id) => {
                            let (table, constraint) = catalog
                                .iterate_tables()
                                .find_map(|table| {
                                    table
                                        .constraints
                                        .iter()
                                        .find(|constraint| constraint.get_id() == *id)
                                        .map(|constraint| (table, constraint))
                                })
                                .expect("view constraint dependency must remain visible");
                            format!(
                                "constraint:{}.{}.{}",
                                catalog.get_schema_name(table.schema_id),
                                table.name,
                                constraint.get_name().expect("stored constraints are named")
                            )
                        }
                    })
                    .collect::<Vec<_>>();
                dependencies.sort();
                CatalogViewInspection {
                    schema: catalog.get_schema_name(view.schema_id).to_owned(),
                    name: view.name.clone(),
                    columns: view
                        .columns
                        .iter()
                        .map(|column| {
                            (
                                column.name.clone(),
                                column.data_type.base.get_postgres_name().into(),
                                column.data_type.typmod,
                            )
                        })
                        .collect(),
                    definition: view.query.to_string(),
                    comment: view.comment.clone(),
                    dependencies,
                }
            })
            .collect::<Vec<_>>();
        views.sort_by(|left, right| (&left.schema, &left.name).cmp(&(&right.schema, &right.name)));

        let mut functions = catalog
            .iterate_functions()
            .map(|function| CatalogFunctionInspection {
                schema: catalog.get_schema_name(function.schema_id).to_owned(),
                name: function.name.clone(),
                argument_count: function.definition.args.as_ref().map_or(0, Vec::len),
                return_type: function
                    .definition
                    .return_type
                    .as_ref()
                    .map(ToString::to_string),
                language: function
                    .definition
                    .language
                    .as_ref()
                    .map(ToString::to_string),
            })
            .collect::<Vec<_>>();
        functions
            .sort_by(|left, right| (&left.schema, &left.name).cmp(&(&right.schema, &right.name)));

        CatalogInspection {
            tables,
            sequences,
            views,
            functions,
        }
    }
}
