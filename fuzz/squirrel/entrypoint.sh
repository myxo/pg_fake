#!/bin/bash
set -euo pipefail

su postgres -s /bin/bash -c "/usr/lib/postgresql/18/bin/pg_ctl \
  -D /var/lib/postgresql/data -l /tmp/postgres.log -w -t 60 start"

export PG_FAKE_SQUIRREL_POSTGRES_URL="postgresql://postgres@localhost/postgres?host=/var/run/postgresql"
export SQUIRREL_CONFIG=/opt/squirrel-config.yml
export AFL_CUSTOM_MUTATOR_LIBRARY=/opt/squirrel-src/build/libpostgresql_mutator.so
export AFL_CUSTOM_MUTATOR_ONLY=1
export AFL_DISABLE_TRIM=1
export AFL_SKIP_CPUFREQ=1
export AFL_I_DONT_CARE_ABOUT_MISSING_CRASHES=1
export AFL_AUTORESUME=1
export AFL_NO_UI=1

mkdir -p /out
exec /opt/squirrel-src/AFLplusplus/afl-fuzz \
  -i /opt/seeds -o /out -m none -V "${AFL_RUN_SECONDS:-300}" \
  -- /opt/fuzz-bin/squirrel_matches_postgres
