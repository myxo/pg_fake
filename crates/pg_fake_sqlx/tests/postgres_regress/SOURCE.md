# PostgreSQL regression SQL

`upstream/` contains SQL tests copied from PostgreSQL 18's core regression suite at commit `74169d3a1d695556ad81ef7a9c256daf0d554da1`.

The files are the behavioral SQL corpus selected from `src/test/regress/sql`. Planner, access-method, physical-storage, catalog, server-administration, replication, and psql-client tests are intentionally excluded.

PostgreSQL `psql` meta-command lines are removed because the differential runner executes SQL through drivers, not through `psql`. The data belonging to the one `\copy FROM STDIN` command is removed with that command. Data belonging to SQL `COPY FROM STDIN` statements remains in the files; the runner records the unsupported SQL statement as a blocker and stops that stateful script before reaching its protocol data. The complete PostgreSQL license for these adapted files is in `POSTGRESQL-COPYRIGHT`.

The runner executes every statement it can from the upstream scripts against PostgreSQL and `pg_fake`. A script is listed in `SKIPPED.txt` when it still has at least one unsupported statement, encoding requirement, removed inline fixture dependency, or semantic mismatch. The test verifies that list, so support changes require an explicit review of the skip status.
