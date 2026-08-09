pub enum Baseline {
    MustPass,
    Pending,
}

pub struct Case {
    pub id: &'static str,
    pub source: &'static str,
    pub setup: &'static [&'static str],
    pub sql: &'static str,
    pub baseline: Baseline,
}

pub struct Feature {
    pub name: &'static str,
    pub cases: &'static [Case],
}

pub const FEATURES: &[Feature] = &[
    Feature {
        name: "query foundations",
        cases: &[Case {
            id: "constant_select",
            source: "subselect.sql:5",
            setup: &[],
            sql: "SELECT 1 AS one",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "bound relation scopes",
        cases: &[Case {
            id: "qualified_alias",
            source: "join.sql:52",
            setup: &[
                "CREATE TABLE scope_rows (id INTEGER)",
                "INSERT INTO scope_rows VALUES (1)",
            ],
            sql: "SELECT row_alias.id FROM scope_rows AS row_alias",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "inner joins",
        cases: &[Case {
            id: "cross_join",
            source: "join.sql:69",
            setup: &[
                "CREATE TABLE join_left (id INTEGER)",
                "CREATE TABLE join_right (id INTEGER)",
                "INSERT INTO join_left VALUES (1)",
                "INSERT INTO join_right VALUES (2)",
            ],
            sql: "SELECT * FROM join_left CROSS JOIN join_right",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "outer joins",
        cases: &[Case {
            id: "left_join",
            source: "subselect.sql:1361",
            setup: &[
                "CREATE TABLE outer_left (id INTEGER)",
                "CREATE TABLE outer_right (id INTEGER)",
                "INSERT INTO outer_left VALUES (1)",
            ],
            sql: "SELECT outer_left.id, outer_right.id FROM outer_left LEFT JOIN outer_right ON outer_left.id = outer_right.id",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "derived and scalar subqueries",
        cases: &[Case {
            id: "scalar_subquery",
            source: "subselect.sql:16",
            setup: &[],
            sql: "SELECT (SELECT 1)",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "subquery predicates",
        cases: &[Case {
            id: "in_subquery",
            source: "subselect.sql:5",
            setup: &[],
            sql: "SELECT 1 WHERE 1 IN (SELECT 1)",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "correlated subqueries",
        cases: &[Case {
            id: "correlated_exists",
            source: "subselect.sql:56",
            setup: &[
                "CREATE TABLE correlated_rows (id INTEGER)",
                "INSERT INTO correlated_rows VALUES (1)",
            ],
            sql: "SELECT id FROM correlated_rows outer_row WHERE EXISTS (SELECT 1 FROM correlated_rows inner_row WHERE inner_row.id = outer_row.id)",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "aggregate functions",
        cases: &[Case {
            id: "count",
            source: "aggregates.sql:27",
            setup: &[
                "CREATE TABLE aggregate_rows (id INTEGER)",
                "INSERT INTO aggregate_rows VALUES (1), (2)",
            ],
            sql: "SELECT count(*) FROM aggregate_rows",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "grouping and having",
        cases: &[Case {
            id: "group_by_having",
            source: "select_having.sql:18",
            setup: &[
                "CREATE TABLE grouped_rows (id INTEGER)",
                "INSERT INTO grouped_rows VALUES (1), (1), (2)",
            ],
            sql: "SELECT id, count(*) FROM grouped_rows GROUP BY id HAVING count(*) > 1",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "distinct",
        cases: &[Case {
            id: "distinct_rows",
            source: "select_distinct.sql:241",
            setup: &[
                "CREATE TABLE distinct_rows (id INTEGER)",
                "INSERT INTO distinct_rows VALUES (1), (1), (2)",
            ],
            sql: "SELECT DISTINCT id FROM distinct_rows ORDER BY id",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "returning",
        cases: &[Case {
            id: "insert_returning",
            source: "returning.sql:11",
            setup: &["CREATE TABLE returning_rows (id INTEGER)"],
            sql: "INSERT INTO returning_rows VALUES (1) RETURNING id",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "query-sourced mutations",
        cases: &[Case {
            id: "insert_select",
            source: "returning.sql:25",
            setup: &[
                "CREATE TABLE mutation_source (id INTEGER)",
                "CREATE TABLE mutation_target (id INTEGER)",
                "INSERT INTO mutation_source VALUES (1)",
            ],
            sql: "INSERT INTO mutation_target SELECT id FROM mutation_source",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "sequences",
        cases: &[Case {
            id: "nextval",
            source: "sequence.sql:112",
            setup: &["CREATE SEQUENCE manifest_sequence"],
            sql: "SELECT nextval('manifest_sequence')",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "serial and identity",
        cases: &[Case {
            id: "identity_default",
            source: "identity.sql:131",
            setup: &["CREATE TABLE identity_rows (id INTEGER GENERATED ALWAYS AS IDENTITY)"],
            sql: "INSERT INTO identity_rows DEFAULT VALUES RETURNING id",
            baseline: Baseline::Pending,
        }],
    },
    Feature {
        name: "foreign-key enforcement",
        cases: &[Case {
            id: "valid_reference",
            source: "foreign_key.sql:24",
            setup: &[
                "CREATE TABLE parent_rows (id INTEGER PRIMARY KEY)",
                "CREATE TABLE child_rows (parent_id INTEGER REFERENCES parent_rows(id))",
                "INSERT INTO parent_rows VALUES (1)",
            ],
            sql: "INSERT INTO child_rows VALUES (1)",
            baseline: Baseline::MustPass,
        }],
    },
    Feature {
        name: "foreign-key actions",
        cases: &[Case {
            id: "delete_cascade",
            source: "foreign_key.sql:488",
            setup: &[
                "CREATE TABLE cascade_parent (id INTEGER PRIMARY KEY)",
                "CREATE TABLE cascade_child (parent_id INTEGER REFERENCES cascade_parent(id) ON DELETE CASCADE)",
                "INSERT INTO cascade_parent VALUES (1)",
                "INSERT INTO cascade_child VALUES (1)",
            ],
            sql: "DELETE FROM cascade_parent WHERE id = 1",
            baseline: Baseline::MustPass,
        }],
    },
    Feature {
        name: "deferred foreign keys",
        cases: &[Case {
            id: "deferred_reference",
            source: "foreign_key.sql:1001",
            setup: &[
                "CREATE TABLE deferred_parent (id INTEGER PRIMARY KEY)",
                "CREATE TABLE deferred_child (parent_id INTEGER REFERENCES deferred_parent(id) DEFERRABLE INITIALLY DEFERRED)",
                "BEGIN",
                "INSERT INTO deferred_child VALUES (1)",
                "INSERT INTO deferred_parent VALUES (1)",
            ],
            sql: "COMMIT",
            baseline: Baseline::MustPass,
        }],
    },
    Feature {
        name: "uuid",
        cases: &[Case {
            id: "uuid_storage",
            source: "uuid.sql:15",
            setup: &[
                "CREATE TABLE uuid_rows (id UUID)",
                "INSERT INTO uuid_rows VALUES ('12345678-1234-1234-1234-123456789abc')",
            ],
            sql: "SELECT id FROM uuid_rows",
            baseline: Baseline::MustPass,
        }],
    },
    Feature {
        name: "date and time",
        cases: &[Case {
            id: "date_time_storage",
            source: "date.sql:32; time.sql:28",
            setup: &[
                "CREATE TABLE date_time_rows (day DATE, at TIME)",
                "INSERT INTO date_time_rows VALUES ('1999-01-08', '23:59:59')",
            ],
            sql: "SELECT day, at FROM date_time_rows",
            baseline: Baseline::MustPass,
        }],
    },
    Feature {
        name: "timestamp and timezone",
        cases: &[Case {
            id: "timestamptz_storage",
            source: "timestamptz.sql:49",
            setup: &[
                "SET TIME ZONE 'UTC'",
                "CREATE TABLE timestamptz_rows (at TIMESTAMPTZ)",
                "INSERT INTO timestamptz_rows VALUES ('1997-02-10 17:32:01-08')",
            ],
            sql: "SELECT at FROM timestamptz_rows",
            baseline: Baseline::MustPass,
        }],
    },
    Feature {
        name: "interval",
        cases: &[Case {
            id: "interval_storage",
            source: "interval.sql:8",
            setup: &[
                "CREATE TABLE interval_rows (span INTERVAL)",
                "INSERT INTO interval_rows VALUES ('1 day 2 hours')",
            ],
            sql: "SELECT span FROM interval_rows",
            baseline: Baseline::MustPass,
        }],
    },
    Feature {
        name: "clock hierarchy",
        cases: &[Case {
            id: "transaction_timestamp",
            source: "timestamp.sql:8",
            setup: &[],
            sql: "SELECT now() = transaction_timestamp()",
            baseline: Baseline::Pending,
        }],
    },
];
