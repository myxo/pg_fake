
use super::*;
use crate::txn::{CommandId, Snapshot, TransactionRegistry};
use crate::value::{BaseType, PgType};
use chaos_theory::check;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_column(name: &str, nullable: bool) -> ColumnDef {
    ColumnDef {
        name: name.into(),
        data_type: PgType::create(BaseType::Int4),
        nullable,
        default: None,
        default_sequence: None,
        identity: None,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn get_constraint_id(constraint: &Constraint) -> ConstraintId {
    match constraint {
        Constraint::PrimaryKey { id, .. }
        | Constraint::Unique { id, .. }
        | Constraint::Check { id, .. } => *id,
        Constraint::ForeignKey(foreign_key) => foreign_key.id,
    }
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn creates_looks_up_and_drops_tables() {
    let mut catalog = Catalog::create();
    let users = catalog
        .create_table(
            "users".into(),
            vec![create_column("id", false), create_column("age", true)],
            vec![],
        )
        .unwrap();
    let posts = catalog
        .create_table("posts".into(), vec![create_column("id", false)], vec![])
        .unwrap();

    assert_eq!(
        catalog.require_schema(DEFAULT_SCHEMA).unwrap().name,
        DEFAULT_SCHEMA
    );
    assert_eq!(users, TableId(1));
    assert_eq!(posts, TableId(2));
    assert_eq!(catalog.require_table("users").unwrap().id, users);
    assert_eq!(
        catalog.require_table("users").unwrap().columns,
        vec![create_column("id", false), create_column("age", true)]
    );

    let dropped = catalog.drop_table("users").unwrap();
    assert_eq!(dropped.id, users);
    assert_eq!(
        catalog.require_table("users").unwrap_err().sqlstate,
        SqlState::UndefinedTable
    );
    assert_eq!(catalog.require_table("posts").unwrap().id, posts);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reports_42p07_for_duplicate_table() {
    let mut catalog = Catalog::create();
    catalog
        .create_table("users".into(), vec![create_column("id", false)], vec![])
        .unwrap();

    let error = catalog
        .create_table("users".into(), vec![create_column("id", false)], vec![])
        .unwrap_err();

    assert_eq!(error.sqlstate, SqlState::DuplicateTable);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reports_42p01_for_missing_table() {
    let mut catalog = Catalog::create();

    assert_eq!(
        catalog.require_table("missing").unwrap_err().sqlstate,
        SqlState::UndefinedTable
    );
    assert_eq!(
        catalog.drop_table("missing").unwrap_err().sqlstate,
        SqlState::UndefinedTable
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn versions_catalog_visibility_and_preserves_relation_identity() {
    let mut transactions = TransactionRegistry::create();
    let mut history = CatalogHistory::create();
    let before_create = Snapshot::create(&transactions);
    let creator = transactions.begin();
    let mut catalog = history.materialize(
        Some(creator),
        before_create.use_command(CommandId(0)),
        &transactions,
    );
    let previous = catalog.clone();
    let table_id = catalog
        .create_table("items".into(), vec![create_column("id", false)], vec![])
        .unwrap();
    history.record_changes(&previous, &catalog, creator, CommandId(0));

    assert!(
        history
            .materialize(
                Some(creator),
                before_create.use_command(CommandId(1)),
                &transactions,
            )
            .require_table("items")
            .is_ok()
    );
    assert!(
        history
            .materialize(None, before_create, &transactions)
            .require_table("items")
            .is_err()
    );

    transactions.commit(creator);
    let after_create = Snapshot::create(&transactions);
    assert_eq!(
        history
            .materialize(None, after_create, &transactions)
            .require_table("items")
            .unwrap()
            .id,
        table_id
    );
    assert!(
        history
            .materialize(None, before_create, &transactions)
            .require_table("items")
            .is_err()
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn discards_aborted_catalog_versions_and_reclaims_created_identity() {
    let mut transactions = TransactionRegistry::create();
    let mut history = CatalogHistory::create();
    let creator = transactions.begin();
    let snapshot = Snapshot::create(&transactions);
    let mut catalog = history.materialize(Some(creator), snapshot, &transactions);
    let previous = catalog.clone();
    let table_id = catalog
        .create_table("items".into(), vec![create_column("id", false)], vec![])
        .unwrap();
    history.record_changes(&previous, &catalog, creator, CommandId(0));

    let reclaimed = history.discard_transaction(creator);
    transactions.abort(creator);

    assert_eq!(reclaimed.tables, vec![table_id]);
    assert!(
        history
            .materialize(None, Snapshot::create(&transactions), &transactions)
            .require_table("items")
            .is_err()
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn assigns_new_relation_and_constraint_identities_after_name_reuse() {
    let mut catalog = Catalog::create();
    let first_table = catalog
        .create_table(
            "items".into(),
            vec![create_column("id", false)],
            vec![Constraint::PrimaryKey {
                id: ConstraintId(0),
                name: "items_pkey".into(),
                columns: vec!["id".into()],
            }],
        )
        .unwrap();
    let first_constraint =
        get_constraint_id(&catalog.require_table("items").unwrap().constraints[0]);
    catalog.drop_table("items").unwrap();
    let second_table = catalog
        .create_table(
            "items".into(),
            vec![create_column("id", false)],
            vec![Constraint::PrimaryKey {
                id: ConstraintId(0),
                name: "items_pkey".into(),
                columns: vec!["id".into()],
            }],
        )
        .unwrap();
    let second_constraint =
        get_constraint_id(&catalog.require_table("items").unwrap().constraints[0]);

    assert_ne!(first_table, second_table);
    assert_ne!(first_constraint, second_constraint);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn versions_schema_visibility_and_identity() {
    let mut transactions = TransactionRegistry::create();
    let mut history = CatalogHistory::create();
    let before_create = Snapshot::create(&transactions);
    let creator = transactions.begin();
    let mut catalog = history.materialize(Some(creator), before_create, &transactions);
    let previous = catalog.clone();
    let first_id = catalog.create_schema("app".into()).unwrap();
    history.record_changes(&previous, &catalog, creator, CommandId(0));

    assert!(
        history
            .materialize(
                Some(creator),
                before_create.use_command(CommandId(1)),
                &transactions,
            )
            .require_schema("app")
            .is_ok()
    );
    assert!(
        history
            .materialize(None, before_create, &transactions)
            .require_schema("app")
            .is_err()
    );

    transactions.commit(creator);
    let before_drop = Snapshot::create(&transactions);
    let changer = transactions.begin();
    let mut catalog = history.materialize(Some(changer), before_drop, &transactions);
    let previous = catalog.clone();
    catalog.drop_schema("app").unwrap();
    history.record_changes(&previous, &catalog, changer, CommandId(0));
    let mut catalog = history.materialize(
        Some(changer),
        before_drop.use_command(CommandId(1)),
        &transactions,
    );
    let previous = catalog.clone();
    let second_id = catalog.create_schema("app".into()).unwrap();
    history.record_changes(&previous, &catalog, changer, CommandId(1));

    assert_ne!(first_id, second_id);
    assert_eq!(
        history
            .materialize(
                Some(changer),
                before_drop.use_command(CommandId(2)),
                &transactions,
            )
            .require_schema("app")
            .unwrap()
            .id,
        second_id
    );
    assert_eq!(
        history
            .materialize(None, before_drop, &transactions)
            .require_schema("app")
            .unwrap()
            .id,
        first_id
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn keeps_the_default_schema_materializable() {
    let mut catalog = Catalog::create();

    assert_eq!(
        catalog.drop_schema(DEFAULT_SCHEMA).unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        catalog.require_schema(DEFAULT_SCHEMA).unwrap().id,
        SchemaId(1)
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn preserves_catalog_snapshot_model_across_generated_name_reuse() {
    check(|source| {
        let commit_create: bool = source.any("commit_create");
        let commit_change: bool = source.any("commit_change");
        let recreate: bool = source.any("recreate");
        let mut transactions = TransactionRegistry::create();
        let mut history = CatalogHistory::create();
        let creator = transactions.begin();
        let before_create = Snapshot::create(&transactions);
        let mut catalog = history.materialize(Some(creator), before_create, &transactions);
        let previous = catalog.clone();
        let first_id = catalog
            .create_table("items".into(), vec![create_column("id", false)], vec![])
            .unwrap();
        history.record_changes(&previous, &catalog, creator, CommandId(0));

        if !commit_create {
            history.discard_transaction(creator);
            transactions.abort(creator);
            assert!(
                history
                    .materialize(None, Snapshot::create(&transactions), &transactions)
                    .require_table("items")
                    .is_err()
            );
            return;
        }

        transactions.commit(creator);
        let retained_snapshot = Snapshot::create(&transactions);
        let reader = transactions.begin();
        transactions.retain_snapshot(reader, retained_snapshot);
        let changer = transactions.begin();
        let mut catalog = history.materialize(Some(changer), retained_snapshot, &transactions);
        let previous = catalog.clone();
        catalog.drop_table("items").unwrap();
        history.record_changes(&previous, &catalog, changer, CommandId(0));
        let second_id = recreate.then(|| {
            let mut catalog = history.materialize(
                Some(changer),
                retained_snapshot.use_command(CommandId(1)),
                &transactions,
            );
            let previous = catalog.clone();
            let id = catalog
                .create_table("items".into(), vec![create_column("id", false)], vec![])
                .unwrap();
            history.record_changes(&previous, &catalog, changer, CommandId(1));
            id
        });

        assert_eq!(
            history
                .materialize(Some(reader), retained_snapshot, &transactions)
                .require_table("items")
                .unwrap()
                .id,
            first_id
        );
        if commit_change {
            transactions.commit(changer);
        } else {
            history.discard_transaction(changer);
            transactions.abort(changer);
        }
        let latest = history.materialize(None, Snapshot::create(&transactions), &transactions);
        if commit_change {
            match second_id {
                Some(id) => assert_eq!(latest.require_table("items").unwrap().id, id),
                None => assert!(latest.require_table("items").is_err()),
            }
        } else {
            assert_eq!(latest.require_table("items").unwrap().id, first_id);
        }
        assert_eq!(
            history
                .materialize(Some(reader), retained_snapshot, &transactions)
                .require_table("items")
                .unwrap()
                .id,
            first_id
        );
    });
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn binds_foreign_keys_and_sequence_owners_to_table_identities() {
    let mut catalog = Catalog::create();
    let parent = catalog
        .create_table(
            "parents".into(),
            vec![create_column("id", false)],
            vec![Constraint::PrimaryKey {
                id: ConstraintId(0),
                name: "parents_pkey".into(),
                columns: vec!["id".into()],
            }],
        )
        .unwrap();
    let child = catalog
        .create_table(
            "children".into(),
            vec![create_column("parent_id", false)],
            vec![Constraint::ForeignKey(ForeignKey {
                id: ConstraintId(0),
                name: "children_parent_id_fkey".into(),
                columns: vec!["parent_id".into()],
                foreign_table: "parents".into(),
                foreign_table_id: TableId(0),
                referred_columns: vec!["id".into()],
                on_delete: ForeignKeyAction::NoAction,
                on_update: ForeignKeyAction::NoAction,
                deferrable: false,
                initially_deferred: false,
                match_kind: None,
                validated: true,
            })],
        )
        .unwrap();
    let Constraint::ForeignKey(foreign_key) =
        &catalog.require_table("children").unwrap().constraints[0]
    else {
        unreachable!()
    };
    assert_eq!(foreign_key.foreign_table_id, parent);
    assert_eq!(catalog.collect_referencing_foreign_keys(parent)[0].0.id, child);

    let mut sequence = SequenceSchema {
        id: SequenceId(0),
        schema_id: SchemaId(0),
        name: "parents_id_seq".into(),
        data_type: BaseType::Int8,
        increment: 1,
        min_value: 1,
        max_value: i64::MAX,
        start_value: 1,
        cycle: false,
        cache: 1,
        owned_by: Some((parent, "id".into())),
    };
    let sequence_id = catalog.create_sequence(sequence.clone()).unwrap();
    sequence.id = sequence_id;
    sequence.schema_id = SchemaId(1);
    assert_eq!(
        catalog.require_sequence("parents_id_seq").unwrap(),
        &sequence
    );
    let snapshot = catalog.clone();
    catalog.drop_table("children").unwrap();
    catalog.drop_owned_sequences(parent);
    assert!(!catalog.has_referencing_foreign_keys(parent));
    assert!(catalog.require_sequence("parents_id_seq").is_err());
    assert_eq!(snapshot.require_table("children").unwrap().id, child);
    assert_eq!(snapshot.collect_referencing_foreign_keys(parent)[0].0.id, child);
    assert_eq!(
        snapshot.require_sequence("parents_id_seq").unwrap(),
        &sequence
    );

    let mut changed_snapshot = snapshot.clone();
    changed_snapshot
        .require_table_mut("parents")
        .unwrap()
        .columns[0]
        .name = "renamed_id".into();
    changed_snapshot.rename_column_dependencies(parent, "id", "renamed_id");
    let mut renamed = changed_snapshot.require_table("parents").unwrap().clone();
    renamed.name = "renamed_parents".into();
    changed_snapshot.replace_table(renamed).unwrap();
    changed_snapshot.rename_table_dependencies(parent, "renamed_parents");
    let changed_foreign_key = &changed_snapshot.collect_referencing_foreign_keys(parent)[0].1;
    assert_eq!(changed_foreign_key.foreign_table.name, "renamed_parents");
    assert_eq!(changed_foreign_key.referred_columns, ["renamed_id"]);
    assert_eq!(
        changed_snapshot
            .require_sequence("parents_id_seq")
            .unwrap()
            .owned_by,
        Some((parent, "renamed_id".into()))
    );
    assert_eq!(
        snapshot.require_table("parents").unwrap().columns[0].name,
        "id"
    );
    let original_foreign_key = &snapshot.collect_referencing_foreign_keys(parent)[0].1;
    assert_eq!(original_foreign_key.foreign_table.name, "parents");
    assert_eq!(original_foreign_key.referred_columns, ["id"]);
    assert_eq!(
        snapshot.require_sequence("parents_id_seq").unwrap(),
        &sequence
    );
    changed_snapshot.create_schema("later".into()).unwrap();
    assert!(snapshot.require_schema("later").is_err());
    assert!(catalog.require_schema("later").is_err());
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn preserves_old_identity_across_transactional_drop_and_recreate() {
    let mut transactions = TransactionRegistry::create();
    let mut history = CatalogHistory::create();
    let creator = transactions.begin();
    let mut catalog = history.materialize(
        Some(creator),
        Snapshot::create(&transactions).use_command(CommandId(0)),
        &transactions,
    );
    let previous = catalog.clone();
    let old_id = catalog
        .create_table("items".into(), vec![create_column("id", false)], vec![])
        .unwrap();
    history.record_changes(&previous, &catalog, creator, CommandId(0));
    transactions.commit(creator);

    let reader = transactions.begin();
    let old_snapshot = Snapshot::create(&transactions);
    transactions.retain_snapshot(reader, old_snapshot);
    let changer = transactions.begin();
    let mut catalog = history.materialize(Some(changer), old_snapshot, &transactions);
    let previous = catalog.clone();
    catalog.drop_table("items").unwrap();
    history.record_changes(&previous, &catalog, changer, CommandId(0));
    let mut catalog = history.materialize(
        Some(changer),
        old_snapshot.use_command(CommandId(1)),
        &transactions,
    );
    let previous = catalog.clone();
    let new_id = catalog
        .create_table("items".into(), vec![create_column("id", false)], vec![])
        .unwrap();
    history.record_changes(&previous, &catalog, changer, CommandId(1));

    assert_ne!(old_id, new_id);
    assert_eq!(
        history
            .materialize(
                Some(changer),
                old_snapshot.use_command(CommandId(2)),
                &transactions,
            )
            .require_table("items")
            .unwrap()
            .id,
        new_id
    );
    assert_eq!(
        history
            .materialize(Some(reader), old_snapshot, &transactions)
            .require_table("items")
            .unwrap()
            .id,
        old_id
    );

    transactions.commit(changer);
    let reclaimed = history.prune(
        transactions.find_reclamation_horizon(),
        &transactions,
        &std::collections::BTreeSet::new(),
    );
    assert!(reclaimed.tables.is_empty());
    assert_eq!(
        history
            .materialize(Some(reader), old_snapshot, &transactions)
            .require_table("items")
            .unwrap()
            .id,
        old_id
    );
    transactions.finish_read_only(reader);
    let reclaimed = history.prune(
        transactions.find_reclamation_horizon(),
        &transactions,
        &std::collections::BTreeSet::new(),
    );
    assert_eq!(reclaimed.tables, vec![old_id]);
    assert_eq!(
        history
            .materialize(None, Snapshot::create(&transactions), &transactions)
            .require_table("items")
            .unwrap()
            .id,
        new_id
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn does_not_apply_deferred_state_to_a_recreated_constraint() {
    let mut catalog = Catalog::create();
    catalog
        .create_table(
            "parents".into(),
            vec![create_column("id", false)],
            vec![Constraint::PrimaryKey {
                id: ConstraintId(0),
                name: "parents_pkey".into(),
                columns: vec!["id".into()],
            }],
        )
        .unwrap();
    let create_child_constraint = || {
        Constraint::ForeignKey(ForeignKey {
            id: ConstraintId(0),
            name: "children_parent_id_fkey".into(),
            columns: vec!["parent_id".into()],
            foreign_table: "parents".into(),
            foreign_table_id: TableId(0),
            referred_columns: vec!["id".into()],
            on_delete: ForeignKeyAction::NoAction,
            on_update: ForeignKeyAction::NoAction,
            deferrable: true,
            initially_deferred: false,
            match_kind: None,
            validated: true,
        })
    };
    catalog
        .create_table(
            "children".into(),
            vec![create_column("parent_id", false)],
            vec![create_child_constraint()],
        )
        .unwrap();
    let old_id = get_constraint_id(&catalog.require_table("children").unwrap().constraints[0]);
    catalog.drop_table("children").unwrap();
    catalog
        .create_table(
            "children".into(),
            vec![create_column("parent_id", false)],
            vec![create_child_constraint()],
        )
        .unwrap();
    let new_id = get_constraint_id(&catalog.require_table("children").unwrap().constraints[0]);

    assert_ne!(old_id, new_id);
    assert!(
        !catalog
            .contains_deferred_foreign_keys(&std::collections::BTreeSet::from([old_id]), false,)
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn versions_sequence_creation_and_abort() {
    let mut transactions = TransactionRegistry::create();
    let mut history = CatalogHistory::create();
    let creator = transactions.begin();
    let before_create = Snapshot::create(&transactions);
    let mut catalog = history.materialize(Some(creator), before_create, &transactions);
    let previous = catalog.clone();
    let sequence_id = catalog
        .create_sequence(SequenceSchema {
            id: SequenceId(0),
            schema_id: SchemaId(0),
            name: "item_ids".into(),
            data_type: BaseType::Int8,
            increment: 1,
            min_value: 1,
            max_value: i64::MAX,
            start_value: 1,
            cycle: false,
            cache: 1,
            owned_by: None,
        })
        .unwrap();
    history.record_changes(&previous, &catalog, creator, CommandId(0));

    assert_eq!(
        history
            .materialize(
                Some(creator),
                before_create.use_command(CommandId(1)),
                &transactions,
            )
            .require_sequence("item_ids")
            .unwrap()
            .id,
        sequence_id
    );
    assert!(
        history
            .materialize(None, before_create, &transactions)
            .require_sequence("item_ids")
            .is_err()
    );
    assert_eq!(
        history.discard_transaction(creator).sequences,
        vec![sequence_id]
    );
    transactions.abort(creator);
    assert!(
        history
            .materialize(None, Snapshot::create(&transactions), &transactions)
            .require_sequence("item_ids")
            .is_err()
    );
}
