#!/usr/bin/env bash
# Private claim on the public chain, sequencer side, step 1 (tools/real_proof/README.md): creates the native schedule
# of tools/real_proof/public-claim.env for the beneficiary the prover's runner made (its public id only), on local-pub,
# with the public-chain wallet (its account labelled rp-public-creator is CV_PUBLIC_CREATOR). The runner's
# beneficiary.txt and beneficiary-pk.txt give the two values.
#
#   tools/real_proof/public_schedule.sh <beneficiary id> <beneficiary public key> <work dir>
#
# Sends: a small native transfer to the beneficiary (so its account exists), then the schedule (total 10^10, cliff
# 60 s after the chain clock, end 3 h later). Appends both to <work dir>/setup.tsv (step, hash, block, what), which
# submit_exported.sh turns into the run's record.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/../../scripts/env.sh"
unset RISC0_DEV_MODE
set -a
# shellcheck disable=SC1091
source "$here/public-claim.env"
set +a
CV_PUBLIC_LABEL="${CV_EXPORT_LABEL:-$CV_PUBLIC_LABEL}"

ben="${1:?usage: $0 <beneficiary id> <beneficiary public key> <work dir>}"; pk="${2:?usage}"; work="${3:?usage}"
[[ "$ben" =~ ^[1-9A-HJ-NP-Za-km-z]{32,44}$ ]] || { echo "not an account id: $ben" >&2; exit 2; }
[[ "$pk" =~ ^[0-9a-f]{64}$ ]] || { echo "not a public key: $pk" >&2; exit 2; }
mkdir -p "$work"
"$here/../../scripts/seq.sh" pub status >/dev/null || { echo "local-pub is not running" >&2; exit 1; }
export LEE_WALLET_HOME_DIR="${CV_PUBLIC_WALLET:-$CV_RUN_DIR/wallet-local-pub}" LEE_WALLET_PASSWORD="${LEE_WALLET_PASSWORD:-}"
export RUST_LOG="${RUST_LOG:-warn}"
CV="${CV_CLI:-$CARGO_TARGET_DIR/debug/cliff-veil}"
creator="$(wallet account list | grep -F '[rp-public-creator]' | grep -oE 'Public/[1-9A-HJ-NP-Za-km-z]{32,44}' | head -1)"
creator="${creator#Public/}"
[ "$creator" = "$CV_PUBLIC_CREATOR" ] || { echo "the wallet's rp-public-creator is '$creator', not $CV_PUBLIC_CREATOR" >&2; exit 1; }

# A public account not used yet is named by its public key (the wallet checks it gives the id).
r="$(wallet auth-transfer send --from cv-payer --to-pk "$pk" --amount 100000000 | grep -E 'Transaction (hash|is included)')"
h="$(sed -n 's/^Transaction hash is \([0-9a-f]\{64\}\)$/\1/p' <<< "$r")"; b="$(sed -n 's/^Transaction is included in block \([0-9]*\)$/\1/p' <<< "$r")"
[ -n "$h" ] && [ -n "$b" ] || { echo "funding the beneficiary failed" >&2; exit 1; }
printf 'fund-beneficiary\t%s\t%s\tnative transfer to the beneficiary (from the local genesis test account)\n' "$h" "$b" >> "$work/setup.tsv"
echo "fund beneficiary: tx $h in block $b"

"$CV" --json create --from rp-public-creator --beneficiary "$ben" --total 10000000000 --start now --cliff +60s --end +3h \
  --label "$CV_PUBLIC_LABEL" | sed -n '/^{/,$p' > "$work/create.json"
cat "$work/create.json"
h="$(jq -r '.sent[0].hash' "$work/create.json")"; b="$(jq -r '.sent[0].block' "$work/create.json")"
s="$(jq -r '.transactions[0].schedules[0] // empty' "$work/create.json")"
[ -z "$s" ] && s="$("$CV" address --creator rp-public-creator --label "$CV_PUBLIC_LABEL")"
printf 'create\t%s\t%s\tvesting: create a native linear schedule (cliff 60 s, end 3 h) for the beneficiary\n' "$h" "$b" >> "$work/setup.tsv"
echo "$s" > "$work/schedule.txt"
echo "schedule $s: tx $h in block $b"
