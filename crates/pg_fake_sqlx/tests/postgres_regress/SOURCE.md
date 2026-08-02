# PostgreSQL regression SQL

`upstream/` contains unmodified SQL files copied from PostgreSQL 18's core regression suite at commit `74169d3a1d695556ad81ef7a9c256daf0d554da1`.

The files are the behavioral SQL corpus selected from `src/test/regress/sql`. Planner, access-method, physical-storage, catalog, server-administration, replication, and psql-client tests are intentionally excluded.

The complete PostgreSQL license for these copied files is in `POSTGRESQL-COPYRIGHT`.

The runner executes every statement it can from the upstream scripts against PostgreSQL and `pg_fake`. A script is listed in `SKIPPED.txt` when it still has at least one unsupported statement, psql dependency, encoding requirement, or semantic mismatch. The test verifies that list, so support changes require an explicit review of the skip status.
