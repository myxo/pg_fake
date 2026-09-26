#!/bin/bash
# Instrument only the crates whose coverage should guide the fuzzer.
# Dependency I/O loops (tokio/mio/sqlx) are timing-nondeterministic, so
# instrumenting them makes AFL coverage unstable.
rustc_bin="$1"
shift

crate=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--crate-name" ]; then
    crate="$arg"
    break
  fi
  prev="$arg"
done

case "$crate" in
  pg_fake | pg_fake_sqlx | sqlparser | squirrel_matches_postgres)
    exec "$rustc_bin" "$@"
    ;;
esac

out=()
while [ $# -gt 0 ]; do
  if [ "$1" = "-C" ]; then
    case "$2" in
      passes=sancov-module | llvm-args=-sanitizer-coverage-*)
        shift 2
        continue
        ;;
    esac
  fi
  out+=("$1")
  shift
done
exec "$rustc_bin" "${out[@]}"
