#!/usr/bin/env bash
# Private claim on the public chain, sequencer side, step 3 (tools/real_proof/README.md): submits the transaction the
# runner proved and kept (public_export.sh), waits for its block, scans the public endpoint's answers for it, and
# writes the record scripts/verify_public.sh re-checks.
#
#   tools/real_proof/submit_exported.sh <runner record dir> <work dir> <evidence dir>
#
#   <runner record dir>  the downloaded artifact: exported-tx.json, export-summary.json, claim.json, ...
#   <work dir>           setup.tsv and schedule.txt of public_schedule.sh
#   <evidence dir>       evidence/public-chain/<utc>-private-real-proof/ (created): run.json, the runner's record,
#                        submit.json (the sequencer's answer), scan.txt, the RPC answers for the claim
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/../../scripts/env.sh"
unset RISC0_DEV_MODE
set -a
# shellcheck disable=SC1091
source "$here/public-claim.env"
set +a
rec="${1:?usage}"; work="${2:?usage}"; ev="${3:?usage}"
SEQ=http://127.0.0.1:3051
die() { echo "FAILED: $*" >&2; exit 1; }
rpc() { curl -fsS -m 30 -H 'Content-Type: application/json' -d "$2" "$1"; }
for f in exported-tx.json export-summary.json claim.json; do [ -f "$rec/$f" ] || die "no $rec/$f"; done
[ "$(sha256sum "$rec/exported-tx.json" | cut -c1-64)" = "$(jq -r .exported_tx_sha256 "$rec/export-summary.json")" ] \
  || die "exported-tx.json does not match its recorded sha256"
[ "$(jq -r .risc0_dev_mode "$rec/export-summary.json")" = false ] || die "the runner proved in dev mode"
[ "$(jq -r .method "$rec/exported-tx.json")" = sendTransaction ] || die "exported-tx.json is not a sendTransaction request"
mkdir -p "$ev"

# The request exactly as the wallet would have posted it.
answer="$(curl -fsS -m 60 -H 'Content-Type: application/json' --data-binary "@$rec/exported-tx.json" "$SEQ")"
echo "$answer" | jq . | tee "$ev/submit.json"
hash="$(jq -r '.result // empty' <<< "$answer")"
[[ "$hash" =~ ^[0-9a-f]{64}$ ]] || die "the sequencer did not accept the transaction: $answer"
block=""
for _ in $(seq 1 24); do
  block="$(rpc "$SEQ" "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getTransaction\",\"params\":[\"$hash\"]}" \
    | jq -r '.result | if type == "array" then .[1] elif type == "object" then (.block_id // .block // empty) else empty end' 2>/dev/null || true)"
  [ -n "$block" ] && [ "$block" != null ] && break
  sleep 5
done
[ -n "$block" ] && [ "$block" != null ] || die "transaction $hash not included within 2 min"
echo "private claim: tx $hash in block $block"

# Read back through the public endpoint, as anyone can.
PA="$(jq -r .canonical_private_account "$rec/export-summary.json")"; PO="$(jq -r .one_time_address "$rec/export-summary.json")"
rpc "$CV_PUBLIC_URL" "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getTransaction\",\"params\":[\"$hash\"]}" > "$ev/private-claim-transaction.json"
rpc "$CV_PUBLIC_URL" "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getBlock\",\"params\":[$block]}" > "$ev/private-claim-block.json"
python3 -I "$CV_REPO/tools/scan_rpc/scan_rpc.py" "$CV_PUBLIC_URL" "$block" "$hash" "$PA" "$PO" | tee "$ev/scan.txt"
canonical="$(sed -n "s/^  RPC_TOTAL $PA //p" "$ev/scan.txt")"
[ "$canonical" = 0 ] || die "the canonical private account appears in the public endpoint's answers ($canonical)"

for f in export-summary.json claim.json claim.time memory.tsv exported-tx.json exported-tx.sha256; do
  [ -f "$rec/$f" ] && cp "$rec/$f" "$ev/"
done
[ -f "$rec/proxy/methods.tsv" ] && cp "$rec/proxy/methods.tsv" "$ev/methods.tsv"

schedule="$(cat "$work/schedule.txt")"
run="$(basename "$ev")"
ids_toml="$(git -C "$CV_REPO" show origin/public-chain:deploy/ids-local-pub.toml 2>/dev/null || cat "$CV_REPO/deploy/ids-local-pub.toml")"
fees_config="$(sed -n 's/^fees_config = "\([1-9A-HJ-NP-Za-km-z]*\)".*/\1/p' <<< "$ids_toml")"
ata="$(sed -n 's/^ata = "\([1-9A-HJ-NP-Za-km-z]*\)".*/\1/p' <<< "$ids_toml")"
{
  cat "$work/setup.tsv"
  printf 'private-claim\t%s\t%s\tvesting: private claim (ClaimAsOf) to a fresh private account, real proof made on a GitHub-hosted runner, submitted here\n' "$hash" "$block"
} | jq -R -s --arg run "$run" --arg schedule "$schedule" --arg ben "$(jq -r .beneficiary "$rec/export-summary.json")" \
    --arg creator "$CV_PUBLIC_CREATOR" --arg vesting "$CV_VESTING" --arg fees "$CV_VESTING_FEES" --arg token "$CV_TOKEN_PROGRAM" \
    --arg ata "$ata" --arg fees_config "$fees_config" --arg amount "$(jq -r .amount "$rec/export-summary.json")" \
    --arg run_url "${CV_PROVER_RUN_URL:-}" '
  [split("\n")[] | select(length > 0) | split("\t")] as $rows
  | {what: "a private claim with a real proof, made on a GitHub-hosted runner against the public read-only endpoint and submitted by the sequencer host",
     run: $run, network: "local-pub", lez: "v0.4.0-rc3", dev_mode: false, prover_run: $run_url,
     programs: ({token: $token, vesting_fees: $fees, vesting: $vesting} + (if $ata != "" then {ata: $ata} else {} end)),
     accounts: (if $fees_config != "" then {fees_config: $fees_config} else {} end),
     transactions: [$rows | to_entries[] | {step: (.key + 1), what: .value[3], hash: .value[1], block: (.value[2] | tonumber)}],
     schedules: [{name: "public-real-proof", account: $schedule,
                  expect: {creator: $creator, beneficiary: $ben, asset: "native", total: 10000000000, claimed: ($amount | tonumber)},
                  expect_from: "the schedule as created, and the amount of the private claim (its only claim)"}],
     refusals: [], queries: []}' > "$ev/run.json"
cat "$ev/run.json"
