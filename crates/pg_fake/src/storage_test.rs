use crate::{
    catalog::{Catalog, ColumnDef},
    value::{BaseType, PgType},
};

use super::*;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_table() -> Table {
    let mut catalog = Catalog::create();
    let table_id = catalog
        .create_table(
            "items".into(),
            vec![ColumnDef {
                name: "value".into(),
                data_type: PgType::create(BaseType::Int4),
                nullable: false,
                default: None,
                default_sequence: None,
                identity: None,
            }],
            vec![],
        )
        .unwrap();
    assert_eq!(table_id.0, 1);
    Table::create(catalog.require_table("items").unwrap().clone())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_indexed_table() -> Table {
    let mut catalog = Catalog::create();
    catalog
        .create_table(
            "items".into(),
            vec![ColumnDef {
                name: "value".into(),
                data_type: PgType::create(BaseType::Int4),
                nullable: false,
                default: None,
                default_sequence: None,
                identity: None,
            }],
            vec![Constraint::PrimaryKey {
                id: crate::catalog::ConstraintId(0),
                name: "values_pkey".into(),
                columns: vec!["value".into()],
            }],
        )
        .unwrap();
    Table::create(catalog.require_table("items").unwrap().clone())
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn creates_new_version_chain_for_insert() {
    let mut table = create_table();
    let row_id = table.insert(Xid(10), CommandId(0), vec![Value::Int4(1)]);

    assert_eq!(row_id, RowId(1));
    assert_eq!(
        table.version_chains.chains.get(&row_id).unwrap().versions,
        vec![RowVersion {
            xmin: Xid(10),
            xmin_command_id: CommandId(0),
            xmax: None,
            xmax_command_id: None,
            row: vec![Value::Int4(1)],
        }]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn finds_visible_rows_through_a_unique_index() {
    let mut table = create_indexed_table();
    let mut transactions = TransactionRegistry::create();
    let xid = transactions.begin();
    let snapshot = Snapshot::create(&transactions);
    let row_id = table.insert(xid, CommandId(0), vec![Value::Int4(1)]);

    assert_eq!(
        table.find_unique_row(&[0], &[Value::Int4(1)], &snapshot, xid, &transactions,),
        Some(row_id)
    );
    assert_eq!(
        table.find_unique_visible_row(&[0], &[Value::Int4(1)], &snapshot, xid, &transactions,),
        Some(&vec![Value::Int4(1)])
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn retires_old_version_and_appends_new_version_for_update() {
    let mut table = create_table();
    let row_id = table.insert(Xid(10), CommandId(0), vec![Value::Int4(1)]);

    assert_eq!(
        table.append_updated_version(
            row_id,
            Xid(10),
            Xid(11),
            CommandId(1),
            vec![Value::Int4(2)],
            None,
        ),
        row_id
    );
    assert_eq!(
        table.version_chains.chains.get(&row_id).unwrap().versions,
        vec![
            RowVersion {
                xmin: Xid(10),
                xmin_command_id: CommandId(0),
                xmax: Some(Xid(11)),
                xmax_command_id: Some(CommandId(1)),
                row: vec![Value::Int4(1)],
            },
            RowVersion {
                xmin: Xid(11),
                xmin_command_id: CommandId(1),
                xmax: None,
                xmax_command_id: None,
                row: vec![Value::Int4(2)],
            },
        ]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn abort_removes_created_versions_and_restores_retired_versions() {
    let mut table = create_table();
    let existing = table.insert(Xid(10), CommandId(0), vec![Value::Int4(1)]);
    let inserted = table.insert(Xid(11), CommandId(1), vec![Value::Int4(2)]);
    table.append_updated_version(
        existing,
        Xid(10),
        Xid(11),
        CommandId(1),
        vec![Value::Int4(3)],
        None,
    );

    table.discard_transaction_versions(Xid(11));

    assert_eq!(
        table.version_chains.chains.get(&existing).unwrap().versions,
        vec![RowVersion {
            xmin: Xid(10),
            xmin_command_id: CommandId(0),
            xmax: None,
            xmax_command_id: None,
            row: vec![Value::Int4(1)],
        }]
    );
    assert!(!table.version_chains.chains.contains_key(&inserted));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn marks_current_version_deleted() {
    let mut table = create_table();
    let row_id = table.insert(Xid(10), CommandId(0), vec![Value::Int4(1)]);
    table.append_updated_version(
        row_id,
        Xid(10),
        Xid(11),
        CommandId(1),
        vec![Value::Int4(2)],
        None,
    );

    assert_eq!(
        table.mark_version_deleted(row_id, Xid(11), Xid(12), CommandId(2)),
        row_id
    );
    assert_eq!(
        table.version_chains.chains.get(&row_id).unwrap().versions,
        vec![
            RowVersion {
                xmin: Xid(10),
                xmin_command_id: CommandId(0),
                xmax: Some(Xid(11)),
                xmax_command_id: Some(CommandId(1)),
                row: vec![Value::Int4(1)],
            },
            RowVersion {
                xmin: Xid(11),
                xmin_command_id: CommandId(1),
                xmax: Some(Xid(12)),
                xmax_command_id: Some(CommandId(2)),
                row: vec![Value::Int4(2)],
            },
        ]
    );
}
