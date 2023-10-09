#!/usr/bin/env bash

# Runs backfill tests.
# NOTE(kwannoel):
# The following scenario is adapted in madsim's integration tests as well.
# But this script reproduces it more reliably (I'm not sure why.)
# Hence keeping it in case we ever need to debug backfill again.

# USAGE:
# ```sh
# cargo make ci-start ci-backfill
# ./ci/scripts/run-backfill-tests.sh
# ```


set -euo pipefail

PARENT_PATH=$(dirname "${BASH_SOURCE[0]}")

run_sql_file() {
  psql -h localhost -p 4566 -d dev -U root -f "$@"
}

run_sql() {
  psql -h localhost -p 4566 -d dev -U root -c "$@"
}

flush() {
  run_sql "FLUSH;"
}

test_basic() {
  run_sql_file "$PARENT_PATH"/sql/backfill/basic/create_base_table.sql

  # Provide snapshot
  run_sql_file "$PARENT_PATH"/sql/backfill/basic/insert.sql
  run_sql_file "$PARENT_PATH"/sql/backfill/basic/insert.sql &
  run_sql_file "$PARENT_PATH"/sql/backfill/basic/create_mv.sql &

  wait
  run_sql_file "$PARENT_PATH"/sql/backfill/basic/select.sql </dev/null
}

test_replication_with_column_pruning() {
   run_sql_file "$PARENT_PATH"/sql/backfill/replication_with_column_pruning/create_base_table.sql
   # Provide snapshot
   run_sql_file "$PARENT_PATH"/sql/backfill/replication_with_column_pruning/insert.sql

   run_sql_file "$PARENT_PATH"/sql/backfill/replication_with_column_pruning/create_mv.sql &

   # Provide upstream updates
   run_sql_file "$PARENT_PATH"/sql/backfill/replication_with_column_pruning/insert.sql &

   wait

   run_sql_file "$PARENT_PATH"/sql/backfill/replication_with_column_pruning/select.sql </dev/null
   run_sql_file "$PARENT_PATH"/sql/backfill/replication_with_column_pruning/drop.sql
}

main() {
  set -euo pipefail
  echo "--- Basic test"
  test_basic
  echo "--- Replication with Column pruning"
  test_replication_with_column_pruning
  echo "Backfill tests complete"
}

main