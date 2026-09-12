
use super::*;

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn transitions_transaction_statuses_from_in_flight_to_final() {
    let mut manager = TransactionRegistry::create();
    let committed = manager.begin();
    let aborted = manager.begin();

    assert_eq!(
        manager.get_status(committed),
        Some(TransactionStatus::InFlight)
    );
    assert_eq!(
        manager.get_status(aborted),
        Some(TransactionStatus::InFlight)
    );

    manager.commit(committed);
    manager.abort(aborted);

    assert_eq!(
        manager.get_status(committed),
        Some(TransactionStatus::Committed(CommitSeq(1)))
    );
    assert_eq!(
        manager.get_status(aborted),
        Some(TransactionStatus::Aborted)
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn allocates_monotonic_xids_and_commit_sequences() {
    let mut manager = TransactionRegistry::create();
    let first = manager.begin();
    let second = manager.begin();

    assert_eq!(first, Xid(1));
    assert_eq!(second, Xid(2));
    assert_eq!(manager.commit(second), CommitSeq(1));
    assert_eq!(manager.commit(first), CommitSeq(2));
    assert_eq!(manager.commit_seq, CommitSeq(2));
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_version(xmin: Xid, xmax: Option<Xid>) -> RowVersion {
    RowVersion {
        xmin,
        xmin_command_id: CommandId(0),
        xmax,
        xmax_command_id: xmax.map(|_| CommandId(0)),
        row: vec![],
    }
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn shows_own_uncommitted_insert_only_to_its_transaction() {
    let mut manager = TransactionRegistry::create();
    let writer = manager.begin();
    let reader = manager.begin();
    let snapshot = Snapshot::create(&manager);
    let inserted = create_version(writer, None);

    assert!(is_visible(&inserted, &snapshot, writer, &manager));
    assert!(!is_visible(&inserted, &snapshot, reader, &manager));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn shows_version_committed_before_snapshot() {
    let mut manager = TransactionRegistry::create();
    let writer = manager.begin();
    manager.commit(writer);
    let reader = manager.begin();
    let snapshot = Snapshot::create(&manager);

    assert!(is_visible(
        &create_version(writer, None),
        &snapshot,
        reader,
        &manager
    ));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn hides_version_committed_after_snapshot() {
    let mut manager = TransactionRegistry::create();
    let writer = manager.begin();
    let reader = manager.begin();
    let snapshot = Snapshot::create(&manager);
    manager.commit(writer);

    assert!(!is_visible(
        &create_version(writer, None),
        &snapshot,
        reader,
        &manager
    ));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn hides_version_deleted_before_snapshot() {
    let mut manager = TransactionRegistry::create();
    let writer = manager.begin();
    manager.commit(writer);
    let deleter = manager.begin();
    manager.commit(deleter);
    let reader = manager.begin();
    let snapshot = Snapshot::create(&manager);

    assert!(!is_visible(
        &create_version(writer, Some(deleter)),
        &snapshot,
        reader,
        &manager
    ));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn keeps_version_visible_during_in_flight_delete() {
    let mut manager = TransactionRegistry::create();
    let writer = manager.begin();
    manager.commit(writer);
    let deleter = manager.begin();
    let reader = manager.begin();
    let snapshot = Snapshot::create(&manager);

    assert!(is_visible(
        &create_version(writer, Some(deleter)),
        &snapshot,
        reader,
        &manager
    ));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn finds_one_visible_version_in_a_chain() {
    let mut manager = TransactionRegistry::create();
    let writer = manager.begin();
    manager.commit(writer);
    let updater = manager.begin();
    let reader = manager.begin();
    let snapshot = Snapshot::create(&manager);
    let chain = RowVersionChain {
        versions: vec![
            create_version(writer, Some(updater)),
            create_version(updater, None),
        ],
    };

    assert_eq!(
        find_visible_version(&chain, &snapshot, reader, &manager),
        Some(&chain.versions[0])
    );
}

#[test]
fn queues_relation_locks_behind_only_earlier_conflicting_waiters() {
    let mut locks = RelationLockManager::create();
    assert!(matches!(
        locks.acquire_many(&[("items".into(), RelationLockMode::RowExclusive)], Xid(1)),
        RelationLockAttempt::Acquired
    ));
    assert!(matches!(
        locks.acquire_many(
            &[("items".into(), RelationLockMode::TableExclusive)],
            Xid(2)
        ),
        RelationLockAttempt::Blocked(_)
    ));
    assert!(matches!(
        locks.acquire_many(&[("items".into(), RelationLockMode::Shared)], Xid(3)),
        RelationLockAttempt::Acquired
    ));
    assert!(matches!(
        locks.acquire_many(&[("items".into(), RelationLockMode::Exclusive)], Xid(4)),
        RelationLockAttempt::Blocked(_)
    ));
    let RelationLockAttempt::Blocked(blockers) =
        locks.acquire_many(&[("items".into(), RelationLockMode::Shared)], Xid(5))
    else {
        panic!("reader must wait behind an earlier access-exclusive request");
    };
    assert_eq!(blockers, vec![Xid(4)]);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn selects_highest_xid_in_wait_for_cycle() {
    let mut graph = WaitForGraph::create();

    assert_eq!(graph.register_wait_dependencies(Xid(1), &[Xid(2)]), None);
    assert_eq!(graph.register_wait_dependencies(Xid(2), &[Xid(3)]), None);
    assert_eq!(
        graph.register_wait_dependencies(Xid(3), &[Xid(1)]),
        Some(Xid(3))
    );
    assert!(graph.take_victim(Xid(3)));
    assert!(!graph.take_victim(Xid(1)));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn breaks_cycle_when_removing_wait_edge() {
    let mut graph = WaitForGraph::create();
    graph.register_wait_dependencies(Xid(4), &[Xid(7)]);
    assert_eq!(
        graph.register_wait_dependencies(Xid(7), &[Xid(4)]),
        Some(Xid(7))
    );

    graph.clear_wait(Xid(7));
    assert_eq!(graph.register_wait_dependencies(Xid(8), &[Xid(4)]), None);
    graph.remove_transaction(Xid(4));
    assert_eq!(graph.register_wait_dependencies(Xid(7), &[Xid(8)]), None);
}
