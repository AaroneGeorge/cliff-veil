#!/usr/bin/env bash
# Runs the end-to-end tests against a running standalone LEZ sequencer.
#
#   e2e/run.sh [extra libtest arguments, e.g. a test name filter]
#
#   CV_SEQ_RPC              sequencer RPC (default http://127.0.0.1:3050, `scripts/seq.sh dev start`)
#   CV_IDS                  program ids file (default deploy/ids-local-dev.toml, `scripts/deploy_local.sh dev`)
#   CV_IDS_FEES             ids of the fee-enabled pair (default deploy/ids-local-dev-fees.toml, `e2e/deploy_fees.sh`)
#   CV_E2E_INCLUDE_PENDING  1: also run the tests marked `#[ignore = "pending: ..."]`
#   CV_E2E_RECORD           a file the tests append measured figures to (batch byte sizes, fees, transaction hashes)
#
# tests/fees_on.rs runs against the fee-enabled pair: deploy it with e2e/deploy_fees.sh first. Its admin test
# renounces that pair's admin, so each run of it needs a fresh pair (run e2e/deploy_fees.sh again).
#
# Every test opens its own funded accounts, so the tests run concurrently against one sequencer (CV_E2E_THREADS,
# default 8). Pending tests (written against SPEC.md, waiting for a program message) are listed by
# `reqmap --pending e2e` and skipped by exact name, so a newly written test runs as soon as its marker is removed.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/../scripts/env.sh"
cd "$CV_REPO"
export CV_SEQ_RPC="${CV_SEQ_RPC:-http://127.0.0.1:3050}"

skip=()
if [ "${CV_E2E_INCLUDE_PENDING:-0}" != 1 ]; then
  while IFS= read -r name; do
    [ -n "$name" ] && skip+=(--skip "$name")
  done < <(cargo run -q --locked -p reqmap -- --pending e2e)
  echo "e2e: skipping ${#skip[@]} pending-test arguments ($(( ${#skip[@]} / 2 )) tests); CV_E2E_INCLUDE_PENDING=1 runs them"
fi

# The CLI test (u3) runs the `cliff-veil` binary.
cargo build --locked -q -p vesting_cli
exec cargo test --locked --no-fail-fast -p vesting_e2e -- --ignored --exact --test-threads "${CV_E2E_THREADS:-8}" "${skip[@]}" "$@"
