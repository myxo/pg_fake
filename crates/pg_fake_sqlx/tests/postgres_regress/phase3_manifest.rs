#[derive(Clone, Copy)]
pub enum BlockerKind {
    Fixture,
    Parser,
    Later,
    Implementation,
}

impl BlockerKind {
    pub fn get_name(self) -> &'static str {
        match self {
            Self::Fixture => "fixture",
            Self::Parser => "parser",
            Self::Later => "later",
            Self::Implementation => "implementation",
        }
    }
}

pub struct Case {
    pub id: &'static str,
    pub source: &'static str,
    pub setup: &'static [&'static str],
    pub sql: &'static str,
    pub blocker: BlockerKind,
}

pub struct Feature {
    pub name: &'static str,
    pub cases: &'static [Case],
}

pub struct Scenario {
    pub feature: &'static str,
    pub name: &'static str,
    pub source: &'static str,
    pub blocker: BlockerKind,
}

pub const FEATURES: &[Feature] = &[
    Feature {
        name: "qualified and temporary relations",
        cases: &[
            Case {
                id: "public_qualified_relation",
                source: "create_table.sql:29 plus focused qualification",
                setup: &[
                    "CREATE TABLE public.phase3_qualified (id INTEGER)",
                    "INSERT INTO public.phase3_qualified VALUES (1)",
                ],
                sql: "SELECT id FROM public.phase3_qualified",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "temporary_shadowing",
                source: "create_table.sql:28 plus focused shadowing",
                setup: &[
                    "CREATE TABLE public.phase3_shadowed (id INTEGER)",
                    "INSERT INTO public.phase3_shadowed VALUES (1)",
                    "CREATE TEMP TABLE phase3_shadowed (id INTEGER)",
                    "INSERT INTO pg_temp.phase3_shadowed VALUES (2)",
                ],
                sql: "SELECT id FROM phase3_shadowed",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "temporary_on_commit_drop",
                source: "temp.sql:55",
                setup: &[
                    "BEGIN",
                    "CREATE TEMP TABLE phase3_on_commit_drop (id INTEGER) ON COMMIT DROP",
                ],
                sql: "COMMIT",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "set operations",
        cases: &[Case {
            id: "union_distinct",
            source: "union.sql:9",
            setup: &[],
            sql: "SELECT 1 AS value UNION SELECT 1 ORDER BY value",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "non-recursive CTEs",
        cases: &[
            Case {
                id: "named_cte",
                source: "with.sql:6",
                setup: &[],
                sql: "WITH values_cte(value) AS (SELECT 1) SELECT value FROM values_cte",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "dependency_chain",
                source: "with.sql:11",
                setup: &[],
                sql: "WITH first_value(value) AS (SELECT 1), second_value(value) AS (SELECT value + 1 FROM first_value) SELECT value FROM second_value",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "repeated_materialized_reference",
                source: "with.sql:14",
                setup: &["CREATE SEQUENCE phase3_cte_sequence"],
                sql: "WITH sampled(value) AS (SELECT nextval('phase3_cte_sequence')) SELECT left_sample.value = right_sample.value FROM sampled AS left_sample CROSS JOIN sampled AS right_sample",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "nested_shadowing",
                source: "with.sql:17",
                setup: &[],
                sql: "WITH values_cte(value) AS (SELECT 1) SELECT (WITH values_cte(value) AS (SELECT 2) SELECT value FROM values_cte) FROM values_cte",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "recursive CTEs",
        cases: &[
            Case {
                id: "recursive_series",
                source: "with.sql:20",
                setup: &[],
                sql: "WITH RECURSIVE series(value) AS (VALUES (1) UNION ALL SELECT value + 1 FROM series WHERE value < 3) SELECT value FROM series ORDER BY value",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "recursive_union_cycle",
                source: "with.sql:81",
                setup: &[
                    "CREATE TABLE phase3_recursive_edges (parent INTEGER, child INTEGER)",
                    "INSERT INTO phase3_recursive_edges VALUES (1, 2), (2, 1)",
                ],
                sql: "WITH RECURSIVE walk(value) AS (VALUES (1) UNION SELECT edges.child FROM walk JOIN phase3_recursive_edges AS edges ON edges.parent = walk.value) SELECT value FROM walk ORDER BY value",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "recursive_empty_seed",
                source: "with.sql:111",
                setup: &[],
                sql: "WITH RECURSIVE empty(value) AS (SELECT 1 WHERE false UNION ALL SELECT value + 1 FROM empty WHERE value < 3) SELECT value FROM empty",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "recursive_reference_in_seed",
                source: "with.sql:220",
                setup: &[],
                sql: "WITH RECURSIVE invalid(value) AS (SELECT value + 1 FROM invalid UNION ALL VALUES (1)) SELECT value FROM invalid",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "data-modifying CTEs",
        cases: &[
            Case {
                id: "insert_returning_cte",
                source: "with.sql:1371",
                setup: &["CREATE TABLE phase3_cte_writes (id INTEGER)"],
                sql: "WITH inserted AS (INSERT INTO phase3_cte_writes VALUES (1) RETURNING id) SELECT id FROM inserted",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "statement_snapshot_visibility",
                source: "with.sql:1391",
                setup: &[
                    "CREATE TABLE phase3_cte_snapshot (id INTEGER)",
                    "INSERT INTO phase3_cte_snapshot VALUES (1)",
                ],
                sql: "WITH inserted AS (INSERT INTO phase3_cte_snapshot VALUES (2) RETURNING id) SELECT id, (SELECT count(*) FROM phase3_cte_snapshot) FROM inserted",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "delete_feeds_insert",
                source: "with.sql:1429",
                setup: &[
                    "CREATE TABLE phase3_cte_move (id INTEGER PRIMARY KEY, value INTEGER)",
                    "INSERT INTO phase3_cte_move VALUES (1, 10)",
                ],
                sql: "WITH removed AS (DELETE FROM phase3_cte_move RETURNING id, value), copied AS (INSERT INTO phase3_cte_move SELECT id + 1, value FROM removed RETURNING id, value) SELECT id, value FROM copied",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "unreferenced_mutation",
                source: "with.sql:1352",
                setup: &["CREATE TABLE phase3_cte_unreferenced (id INTEGER)"],
                sql: "WITH unused AS (INSERT INTO phase3_cte_unreferenced VALUES (1)) SELECT 1",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "referenced_without_returning",
                source: "with.sql:1703",
                setup: &["CREATE TABLE phase3_cte_no_returning (id INTEGER)"],
                sql: "WITH inserted AS (INSERT INTO phase3_cte_no_returning VALUES (1)) SELECT * FROM inserted",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "ON CONFLICT DO NOTHING",
        cases: &[Case {
            id: "unique_arbiter",
            source: "insert_conflict.sql:93",
            setup: &[
                "CREATE TABLE phase3_conflict_nothing (id INTEGER PRIMARY KEY)",
                "INSERT INTO phase3_conflict_nothing VALUES (1)",
            ],
            sql: "INSERT INTO phase3_conflict_nothing VALUES (1) ON CONFLICT (id) DO NOTHING RETURNING id",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "ON CONFLICT DO UPDATE",
        cases: &[Case {
            id: "excluded_row",
            source: "insert_conflict.sql:96",
            setup: &[
                "CREATE TABLE phase3_conflict_update (id INTEGER PRIMARY KEY, value TEXT)",
                "INSERT INTO phase3_conflict_update VALUES (1, 'old')",
            ],
            sql: "INSERT INTO phase3_conflict_update VALUES (1, 'new') ON CONFLICT (id) DO UPDATE SET value = excluded.value RETURNING value",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "window ranking",
        cases: &[Case {
            id: "row_number",
            source: "window.sql:47",
            setup: &[
                "CREATE TABLE phase3_window_rank (value INTEGER)",
                "INSERT INTO phase3_window_rank VALUES (2), (1)",
            ],
            sql: "SELECT value, row_number() OVER (ORDER BY value) FROM phase3_window_rank ORDER BY value",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "window offset and value functions",
        cases: &[Case {
            id: "lag",
            source: "window.sql:61",
            setup: &[
                "CREATE TABLE phase3_window_offset (value INTEGER)",
                "INSERT INTO phase3_window_offset VALUES (1), (2)",
            ],
            sql: "SELECT value, lag(value) OVER (ORDER BY value) FROM phase3_window_offset ORDER BY value",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "window aggregate frames",
        cases: &[Case {
            id: "running_sum",
            source: "window.sql:479",
            setup: &[
                "CREATE TABLE phase3_window_frame (value INTEGER)",
                "INSERT INTO phase3_window_frame VALUES (1), (2), (3)",
            ],
            sql: "SELECT sum(value) OVER (ORDER BY value ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM phase3_window_frame ORDER BY value",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "json",
        cases: &[
            Case {
                id: "json_text",
                source: "json.sql:2",
                setup: &[],
                sql: "SELECT '{ \"value\" : 1 }'::json",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "json_storage_fidelity",
                source: "focused JSON storage fixture",
                setup: &["CREATE TABLE phase3_json_storage (payload JSON NOT NULL)"],
                sql: "INSERT INTO phase3_json_storage VALUES ('{ \"z\" : 1e+02, \"z\" : -0.00, \"nested\" : [true, null, \"Привет\"] }') RETURNING payload",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "json_malformed_input",
                source: "json.sql:18",
                setup: &[],
                sql: "SELECT '[1,]'::json",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "json_equality_rejected",
                source: "json.sql:181",
                setup: &[],
                sql: "SELECT '{}'::json = '{}'::json",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "jsonb",
        cases: &[
            Case {
                id: "jsonb_normalization",
                source: "jsonb.sql:10",
                setup: &[],
                sql: "SELECT '{ \"value\" : 1 }'::jsonb",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "jsonb_migration_storage",
                source: "local:jsonb_migration_storage",
                setup: &["CREATE TABLE phase3_jsonb_storage (payload JSONB NOT NULL UNIQUE)"],
                sql: "INSERT INTO phase3_jsonb_storage VALUES ('{\"amount\":{\"currency\":\"USD\",\"value\":\"12.50\"}}') RETURNING payload",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "jsonb_numeric_equality",
                source: "local:jsonb_numeric_equality",
                setup: &[],
                sql: "SELECT '{\"a\":1.00,\"b\":2}'::jsonb = '{\"b\":2,\"a\":1e0}'::jsonb",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "json and jsonb operators",
        cases: &[Case {
            id: "jsonb_extraction",
            source: "jsonb.sql:215",
            setup: &[],
            sql: "SELECT '{\"value\": 1}'::jsonb ->> 'value'",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "array type and I/O",
        cases: &[Case {
            id: "array_literal",
            source: "arrays.sql:276",
            setup: &[],
            sql: "SELECT ARRAY[1, NULL, 3]",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "array expressions",
        cases: &[Case {
            id: "array_containment",
            source: "arrays.sql:344",
            setup: &[],
            sql: "SELECT ARRAY[1, 2] @> ARRAY[1]",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "MVCC-versioned catalog",
        cases: &[Case {
            id: "own_uncommitted_table",
            source: "transactions.sql:10",
            setup: &["BEGIN", "CREATE TABLE phase3_catalog_mvcc (id INTEGER)"],
            sql: "SELECT * FROM phase3_catalog_mvcc",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "transactional DDL",
        cases: &[Case {
            id: "create_then_rollback",
            source: "transactions.sql:120",
            setup: &["BEGIN", "CREATE TABLE phase3_catalog_ddl (id INTEGER)"],
            sql: "ROLLBACK",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "migration ALTER TABLE",
        cases: &[
            Case {
                id: "alter_column_rewrite",
                source: "alter_table.sql:1360 plus focused migration fixture",
                setup: &[
                    "CREATE TABLE phase3_alter_columns (id INTEGER, value INTEGER)",
                    "INSERT INTO phase3_alter_columns VALUES (1, 4)",
                ],
                sql: "ALTER TABLE phase3_alter_columns ALTER COLUMN value TYPE BIGINT USING value * 10",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "add_not_valid_check",
                source: "alter_table.sql:711",
                setup: &[
                    "CREATE TABLE phase3_alter_check (value INTEGER)",
                    "INSERT INTO phase3_alter_check VALUES (-1)",
                ],
                sql: "ALTER TABLE phase3_alter_check ADD CONSTRAINT positive CHECK (value > 0) NOT VALID",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "validate_foreign_key",
                source: "alter_table.sql:987 plus focused migration fixture",
                setup: &[
                    "CREATE TABLE phase3_alter_parent (id INTEGER PRIMARY KEY)",
                    "INSERT INTO phase3_alter_parent VALUES (1)",
                    "CREATE TABLE phase3_alter_child (parent_id INTEGER)",
                    "INSERT INTO phase3_alter_child VALUES (1)",
                    "ALTER TABLE phase3_alter_child ADD CONSTRAINT parent_fk FOREIGN KEY (parent_id) REFERENCES phase3_alter_parent(id) NOT VALID",
                ],
                sql: "ALTER TABLE phase3_alter_child VALIDATE CONSTRAINT parent_fk",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "index DDL and partial unique indexes",
        cases: &[
            Case {
                id: "covering_partial_index",
                source: "focused migration index fixture",
                setup: &[
                    "CREATE TABLE phase3_index_covering (tenant_id INTEGER, state TEXT, deleted_at INTEGER, payload TEXT)",
                ],
                sql: "CREATE INDEX phase3_index_covering_idx ON phase3_index_covering (tenant_id ASC, state DESC) INCLUDE (payload) WHERE deleted_at IS NULL",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "partial_unique_arbiter",
                source: "focused migration partial-unique fixture",
                setup: &[
                    "CREATE TABLE phase3_index_arbiter (account_id INTEGER, active BOOLEAN, value INTEGER)",
                    "CREATE UNIQUE INDEX phase3_index_arbiter_idx ON phase3_index_arbiter (account_id) WHERE active",
                    "INSERT INTO phase3_index_arbiter VALUES (1, true, 10)",
                ],
                sql: "INSERT INTO phase3_index_arbiter VALUES (1, true, 20) ON CONFLICT (account_id) WHERE active DO UPDATE SET value = excluded.value",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "rename_index_if_exists",
                source: "focused migration index-rename fixture",
                setup: &[
                    "CREATE TABLE phase3_index_rename (id INTEGER)",
                    "CREATE INDEX phase3_index_old_name ON phase3_index_rename (id)",
                ],
                sql: "ALTER INDEX IF EXISTS phase3_index_old_name RENAME TO phase3_index_new_name",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "drop_index_if_exists",
                source: "drop_if_exists.sql:31 plus focused migration fixture",
                setup: &[
                    "CREATE TABLE phase3_index_drop (id INTEGER)",
                    "CREATE INDEX phase3_index_drop_idx ON phase3_index_drop (id)",
                ],
                sql: "DROP INDEX IF EXISTS phase3_index_drop_idx",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "ordinary views",
        cases: &[
            Case {
                id: "select_from_view",
                source: "create_view.sql:29",
                setup: &[
                    "CREATE TABLE phase3_view_source (id INTEGER)",
                    "INSERT INTO phase3_view_source VALUES (1)",
                    "CREATE VIEW phase3_view AS SELECT id FROM phase3_view_source",
                ],
                sql: "SELECT id FROM phase3_view",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "nested_view_with_explicit_columns",
                source: "create_view.sql:169 plus focused column aliases",
                setup: &[
                    "CREATE TABLE phase3_nested_source (id INTEGER, value TEXT)",
                    "INSERT INTO phase3_nested_source VALUES (1, 'one'), (2, 'two')",
                    "CREATE VIEW phase3_inner_view (key, label) AS SELECT id, value FROM phase3_nested_source WHERE id > 1",
                    "CREATE VIEW phase3_outer_view AS SELECT key, label FROM phase3_inner_view",
                ],
                sql: "SELECT key, label FROM phase3_outer_view",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "replace_and_comment_view",
                source: "create_view.sql:312 plus focused COMMENT ON VIEW",
                setup: &[
                    "CREATE TABLE phase3_replace_source (id INTEGER)",
                    "CREATE VIEW phase3_replace_view AS SELECT id FROM phase3_replace_source",
                    "COMMENT ON VIEW phase3_replace_view IS 'compatibility view'",
                ],
                sql: "CREATE OR REPLACE VIEW phase3_replace_view AS SELECT id FROM phase3_replace_source WHERE id > 0",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "drop_view_if_exists",
                source: "drop_if_exists.sql plus focused migration fixture",
                setup: &["CREATE VIEW phase3_drop_view AS SELECT 1 AS id"],
                sql: "DROP VIEW IF EXISTS phase3_drop_view",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "savepoints",
        cases: &[Case {
            id: "create_savepoint",
            source: "transactions.sql:73",
            setup: &["BEGIN"],
            sql: "SAVEPOINT phase3_savepoint",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "session GUC registry",
        cases: &[
            Case {
                id: "set_lock_timeout",
                source: "focused local GUC scenario",
                setup: &[],
                sql: "SET lock_timeout = '10ms'",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "show_lock_timeout",
                source: "focused local GUC scenario",
                setup: &[],
                sql: "SHOW lock_timeout",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "transaction-local GUCs",
        cases: &[
            Case {
                id: "set_local_timezone",
                source: "json.sql:154",
                setup: &["BEGIN"],
                sql: "SET LOCAL TIME ZONE 'UTC'",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "set_local_lock_timeout",
                source: "focused local GUC scenario",
                setup: &["BEGIN"],
                sql: "SET LOCAL lock_timeout = '10ms'",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "set_local_statement_timeout",
                source: "focused migration coordination scenario",
                setup: &["BEGIN"],
                sql: "SET LOCAL statement_timeout = '30min'",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "migration table locks",
        cases: &[
            Case {
                id: "access_exclusive_table_lock",
                source: "lock.sql:32 plus focused qualified migration fixture",
                setup: &[
                    "CREATE TABLE public.phase3_access_exclusive_lock (id INTEGER)",
                    "BEGIN",
                ],
                sql: "LOCK TABLE public.phase3_access_exclusive_lock IN ACCESS EXCLUSIVE MODE",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "exclusive_multi_table_lock",
                source: "lock.sql:31 plus focused multi-relation migration fixture",
                setup: &[
                    "CREATE TABLE public.phase3_exclusive_lock_a (id INTEGER)",
                    "CREATE TABLE public.phase3_exclusive_lock_b (id INTEGER)",
                    "BEGIN",
                ],
                sql: "LOCK TABLE public.phase3_exclusive_lock_a, public.phase3_exclusive_lock_b IN EXCLUSIVE MODE",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "procedural migrations and triggers",
        cases: &[
            Case {
                id: "before_insert_trigger",
                source: "triggers.sql:29 plus focused migration fixture",
                setup: &[
                    "CREATE TABLE phase3_trigger_items (id INTEGER, value BIGINT NOT NULL)",
                    "CREATE FUNCTION phase3_trigger_increment() RETURNS TRIGGER AS $$ BEGIN NEW.value := NEW.value + 1; RETURN NEW; END; $$ LANGUAGE plpgsql",
                    "CREATE TRIGGER phase3_trigger_increment BEFORE INSERT ON phase3_trigger_items FOR EACH ROW EXECUTE FUNCTION phase3_trigger_increment()",
                ],
                sql: "INSERT INTO phase3_trigger_items VALUES (1, 4) RETURNING value",
                blocker: BlockerKind::Implementation,
            },
            Case {
                id: "anonymous_do_control_flow",
                source: "focused procedural migration fixture",
                setup: &["CREATE TABLE phase3_do_results (value BIGINT, message TEXT)"],
                sql: "DO $$ DECLARE affected BIGINT := 1; message TEXT := 'ready'; BEGIN SELECT affected, message INTO affected, message; IF affected = 1 AND message IS NOT NULL THEN INSERT INTO phase3_do_results VALUES (affected, message); ELSIF affected IS NULL THEN UPDATE phase3_do_results SET value = 2; ELSE DELETE FROM phase3_do_results; END IF; GET DIAGNOSTICS affected = ROW_COUNT; END; $$",
                blocker: BlockerKind::Implementation,
            },
        ],
    },
    Feature {
        name: "SELECT row locks",
        cases: &[Case {
            id: "no_key_update_nowait",
            source: "focused local row-lock scenario",
            setup: &[
                "CREATE TABLE phase3_row_lock (id INTEGER PRIMARY KEY)",
                "INSERT INTO phase3_row_lock VALUES (1)",
            ],
            sql: "SELECT * FROM phase3_row_lock FOR NO KEY UPDATE NOWAIT",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "serializable dependency tracking",
        cases: &[Case {
            id: "serializable_transaction",
            source: "transactions.sql:39",
            setup: &[],
            sql: "BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            blocker: BlockerKind::Implementation,
        }],
    },
    Feature {
        name: "serializable validation",
        cases: &[Case {
            id: "serializable_setting",
            source: "transactions.sql:63",
            setup: &["BEGIN"],
            sql: "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            blocker: BlockerKind::Implementation,
        }],
    },
];

pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        feature: "ON CONFLICT DO NOTHING",
        name: "concurrent_unique_insert",
        source: "focused two-session scenario",
        blocker: BlockerKind::Implementation,
    },
    Scenario {
        feature: "ON CONFLICT DO UPDATE",
        name: "concurrent_conflict_recheck",
        source: "focused two-session scenario",
        blocker: BlockerKind::Implementation,
    },
    Scenario {
        feature: "MVCC-versioned catalog",
        name: "uncommitted_ddl_visibility",
        source: "transactions.sql:120 plus focused two-session scenario",
        blocker: BlockerKind::Implementation,
    },
    Scenario {
        feature: "transactional DDL",
        name: "drop_and_recreate_visibility",
        source: "transactions.sql:120 plus focused two-session scenario",
        blocker: BlockerKind::Implementation,
    },
    Scenario {
        feature: "savepoints",
        name: "rollback_releases_subtransaction_locks",
        source: "transactions.sql:73 plus focused two-session scenario",
        blocker: BlockerKind::Implementation,
    },
    Scenario {
        feature: "transaction-local GUCs",
        name: "savepoint_local_setting_restore",
        source: "transactions.sql:73 plus focused session scenario",
        blocker: BlockerKind::Implementation,
    },
    Scenario {
        feature: "SELECT row locks",
        name: "lock_mode_compatibility_matrix",
        source: "lock.sql plus focused two-session scenario",
        blocker: BlockerKind::Implementation,
    },
    Scenario {
        feature: "SELECT row locks",
        name: "skip_locked_work_queue",
        source: "limit.sql:179 plus focused two-session scenario",
        blocker: BlockerKind::Implementation,
    },
    Scenario {
        feature: "serializable dependency tracking",
        name: "write_skew_dependency_graph",
        source: "focused two-session scenario",
        blocker: BlockerKind::Implementation,
    },
    Scenario {
        feature: "serializable validation",
        name: "phantom_insert_serialization_failure",
        source: "focused two-session scenario",
        blocker: BlockerKind::Implementation,
    },
];

pub const LIMITATIONS: &[Scenario] = &[
    Scenario {
        feature: "data-modifying CTEs",
        name: "upstream_relation_fixture",
        source: "with.sql:383",
        blocker: BlockerKind::Fixture,
    },
    Scenario {
        feature: "array type and I/O",
        name: "multidimensional_arrays",
        source: "arrays.sql:121",
        blocker: BlockerKind::Later,
    },
    Scenario {
        feature: "json and jsonb operators",
        name: "SQL_JSON_path_surface",
        source: "jsonpath.sql:1",
        blocker: BlockerKind::Later,
    },
    Scenario {
        feature: "parser coverage",
        name: "valid_PostgreSQL_syntax_without_sqlparser_AST",
        source: "record when encountered by the corpus runner",
        blocker: BlockerKind::Parser,
    },
];
