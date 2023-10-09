#!/usr/bin/env bash

# Exits as soon as any line fails.
set -euo pipefail

source ci/scripts/common.sh

set +e
# Set features, depending on our workflow
# If sqlsmith files are modified, we run tests with sqlsmith enabled.
MATCHES="ci/scripts/cron-fuzz-test.sh\
\|ci/scripts/pr-fuzz-test.sh\
\|ci/scripts/run-fuzz-test.sh\
\|src/tests/sqlsmith"
NOT_MATCHES="\.md"
CHANGED=$(git diff --name-only origin/main | grep -v "$NOT_MATCHES" | grep "$MATCHES")
set -e

# NOTE(kwannoel): Disabled because there's some breakage after #12485,
# see https://github.com/risingwavelabs/risingwave/issues/12577.
# Frontend is relatively stable, e2e fuzz test will cover the same cases also,
# so we can just disable it.
export RUN_SQLSMITH_FRONTEND=0
export RUN_SQLSMITH=1
export SQLSMITH_COUNT=100

# Run e2e tests if changes to sqlsmith source files detected.
if [[ -n "$CHANGED" ]]; then
    echo "--- Checking whether to run all sqlsmith tests"
    echo "origin/main SHA: $(git rev-parse origin/main)"
    echo "Changes to Sqlsmith source files detected:"
    echo "$CHANGED"
    export RUN_SQLSMITH=1
    export SQLSMITH_COUNT=100
    export TEST_NUM=32
    echo "Enabled Sqlsmith tests."
else
    export RUN_SQLSMITH=0
fi

source ci/scripts/run-fuzz-test.sh
