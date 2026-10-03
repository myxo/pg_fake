# Unsupported PostgreSQL features

Permanent scope exclusions; no later phase is planned. Explicit requests fail
in normal and strict modes before mutation. Existing `ANALYZE` and registered
planner-setting tolerance remains unchanged; strict mode rejects those no-ops.

- Server/wire protocol, persistence, WAL/recovery, replication and server administration.
- Physical storage: tablespaces, table access-method selection, compression/TOAST,
  storage parameters, page/file/size inspection and physical statistics.
- `VACUUM`, `REINDEX`, `CLUSTER`, `CHECKPOINT` and extended statistics objects.
- `CREATE INDEX CONCURRENTLY`, `DROP INDEX CONCURRENTLY` and `TABLESAMPLE SYSTEM`.
- Security, authentication, roles, ownership, privileges, RLS/policies, security
  labels, routine/view security options, leakproof attributes and privilege inspection.
- Partitioning, table inheritance and typed tables (`CREATE TABLE ... OF`).
- Full-text search, XML and geometric types/operators.
- Non-UTF-8 text encodings and encoding conversions.
- Collations beyond C/POSIX, English and Russian; arbitrary collation tailoring.
- Regional formatting beyond C/POSIX, US English and Russian.
- Extension loading/management, native code loading and languages beyond SQL/PL/pgSQL.
- Foreign data wrappers, foreign tables/servers, user mappings and foreign schema import.
- Custom base types, operators, casts, operator classes/families and access methods.
- Advanced custom aggregates: ordered/hypothetical sets, polymorphic/variadic
  signatures, internal state and options beyond `SFUNC`, `STYPE`, `INITCOND`,
  `FINALFUNC` and equivalent legacy `BASETYPE` syntax.
- Explicit rewrite rules and login event triggers.
- Large objects (`lo_*`, `loread`, `lowrite` and related APIs).
- Two-phase commit (`PREPARE TRANSACTION`, `COMMIT PREPARED`, `ROLLBACK PREPARED`).
- Direct system-catalog writes or structural changes.
- SQL-visible `xmin`, `xmax`, `cmin`, `cmax`, `ctid` and physical TID operations.
- Transaction-ID/snapshot inspection: `pg_current_xact_id*`, `pg_xact_status`,
  `pg_current_snapshot`, `pg_snapshot_*`, `pg_visible_in_snapshot`, `age(xid)`,
  `mxid_age`, `pg_get_multixact_members` and legacy `txid_*` equivalents.
- SQL filesystem/network/OS access, `COPY` files/`PROGRAM` and psql commands.

## Fidelity limits

- PostgreSQL physical plans, `EXPLAIN` output and performance are not reproduced.
- Exact generated error wording, numeric/float rendering and real-clock values
  are best effort; unspecified row/evaluation order is not guaranteed.
- Generated object IDs and sampled row identities need not match across engines.

## Decisions still open

- Language transforms; remaining unlisted PL/pgSQL forms; standalone shell types
  and transaction-ID types (`xid`, `cid`, `xid8`).
