use super::{Catalog, RelationName, TableId, TableSchema};
use sqlparser::ast;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ConstraintId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Constraint {
    PrimaryKey {
        id: ConstraintId,
        name: String,
        columns: Vec<String>,
    },
    Unique {
        id: ConstraintId,
        name: String,
        columns: Vec<String>,
    },
    Check {
        id: ConstraintId,
        name: String,
        expression: Box<ast::Expr>,
        validated: bool,
    },
    ForeignKey(ForeignKey),
}

impl Constraint {
    pub(crate) fn get_id(&self) -> ConstraintId {
        match self {
            Self::PrimaryKey { id, .. } | Self::Unique { id, .. } | Self::Check { id, .. } => *id,
            Self::ForeignKey(foreign_key) => foreign_key.id,
        }
    }

    pub(crate) fn get_name(&self) -> Option<&str> {
        match self {
            Self::PrimaryKey { name, .. } | Self::Unique { name, .. } => Some(name),
            Self::Check { name, .. } => Some(name),
            Self::ForeignKey(foreign_key) => Some(&foreign_key.name),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForeignKeyAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForeignKey {
    pub(crate) id: ConstraintId,
    pub(crate) name: String,
    pub(crate) columns: Vec<String>,
    pub(crate) foreign_table: RelationName,
    pub(crate) foreign_table_id: TableId,
    pub(crate) referred_columns: Vec<String>,
    pub(crate) on_delete: ForeignKeyAction,
    pub(crate) on_update: ForeignKeyAction,
    pub(crate) deferrable: bool,
    pub(crate) initially_deferred: bool,
    pub(crate) match_kind: Option<ast::ConstraintReferenceMatchKind>,
    pub(crate) validated: bool,
}

impl Catalog {
    pub(crate) fn allocate_constraint_id(&mut self) -> ConstraintId {
        let id = ConstraintId(self.next_constraint_id);
        self.next_constraint_id += 1;
        id
    }

    pub(crate) fn has_constraint(&self, table_id: TableId, constraint_id: ConstraintId) -> bool {
        self.require_table_by_id(table_id).is_ok_and(|table| {
            table
                .constraints
                .iter()
                .any(|constraint| constraint.get_id() == constraint_id)
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn contains_deferred_foreign_keys(
        &self,
        deferred_constraints: &std::collections::BTreeSet<ConstraintId>,
        defer_all: bool,
    ) -> bool {
        self.relations
            .deferrable_foreign_keys
            .iter()
            .any(|(id, initially_deferred)| {
                defer_all || *initially_deferred || deferred_constraints.contains(id)
            })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn collect_referencing_foreign_keys(
        &self,
        parent: TableId,
    ) -> Vec<(TableSchema, ForeignKey)> {
        self.relations
            .referencing_foreign_keys
            .get(&parent)
            .into_iter()
            .flatten()
            .map(|(table, constraint)| {
                let schema = self
                    .require_table_by_id(*table)
                    .expect("foreign key metadata references an existing table");
                let Constraint::ForeignKey(foreign_key) = &schema.constraints[*constraint] else {
                    unreachable!("foreign key metadata references a foreign key")
                };
                (schema.clone(), foreign_key.clone())
            })
            .collect()
    }

    pub(crate) fn has_referencing_foreign_keys(&self, parent: TableId) -> bool {
        self.relations
            .referencing_foreign_keys
            .contains_key(&parent)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn rebuild_foreign_key_metadata(&mut self) {
        let mut deferrable_foreign_keys = Vec::new();
        let mut referencing_foreign_keys: BTreeMap<TableId, Vec<(TableId, usize)>> =
            BTreeMap::new();
        for table in self.iterate_tables() {
            for (index, constraint) in table.constraints.iter().enumerate() {
                let Constraint::ForeignKey(foreign_key) = constraint else {
                    continue;
                };
                if foreign_key.deferrable {
                    deferrable_foreign_keys.push((foreign_key.id, foreign_key.initially_deferred));
                }
                referencing_foreign_keys
                    .entry(foreign_key.foreign_table_id)
                    .or_default()
                    .push((table.id, index));
            }
        }
        let relations = Arc::make_mut(&mut self.relations);
        relations.deferrable_foreign_keys = deferrable_foreign_keys;
        relations.referencing_foreign_keys = referencing_foreign_keys;
    }
}
