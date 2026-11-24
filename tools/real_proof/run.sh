#!/usr/bin/env bash
# A private claim with a real RISC Zero proof, on a standalone LEZ v0.4.0-rc3 sequencer that verifies proofs, recorded
# step by step; then a public claim and a cancel of the same schedule; then (CV_REAL_PROOF_TOKEN=1, the default) a
# token claim to a private holding. See tools/real_proof/README.md.
#
#   tools/real_proof/run.sh <out dir>
#
# Needs: `scripts/seq.sh pub start` (RISC0_DEV_MODE unset in the sequencer's environment), `scripts/deploy_local.sh pub`
# (deploy/ids-local-pub.toml), r0vm 3.0.5 on PATH (scripts/bootstrap.sh), GNU time at /usr/bin/time, jq, python3.
# Memory: the proof needs about 10 GiB of RAM (plus swap as a margin); do not run it on a machine without that much free.
#
# Writes into <out dir>:
#   run.log                       every command and its full output, in order
#   summary.json                  the record: machine, sequencer, programs, every transaction (hash, block), the
#                                 private claim's wall time and peak memory, balances, the RPC scan
#   private-claim.json            the CLI's JSON answer for the proved claim
#   private-claim.time            /usr/bin/time -v of the proved claim (wall time, maximum resident set size)
#   memory.tsv                    5 s samples during the proved claim: available memory, swap, RSS of r0vm and the CLI
#   private-claim-transaction.json, private-claim-block.json
#                                 the RPC answers getTransaction and getBlock for the proved claim, as any client sees
#                                 them (the transaction carries the proof)
#   scan.txt                      tools/scan_rpc/scan_rpc.py on those answers
#   control-dev-receipt.json      a claim proved in dev mode, sent to the same sequencer first: it must be refused
#   control-sequencer-log.txt     the sequencer's log line that names that transaction and the invalid proof
#   token-*.json, token-scan.txt, token.time, memory-token.tsv
#                                 the token part (CV_REAL_PROOF_TOKEN=1, the default; 0 skips it): a token claim to a
#                                 private holding, after funding a private fee payer and initialising the holding,
#                                 each of the three proved (e2e/examples/real_proof_token.rs)
# Local paths are replaced by <build> and ~ in every file.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/../../scripts/env.sh"
# env.sh turns dev mode on for local work; this run proves for real.
unset RISC0_DEV_MODE

out="${1:?usage: $0 <out dir>}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
# Everything goes to run.log as well; on exit (success or not) the log is closed and local paths are scrubbed.
exec 3>&1 4>&2
exec > >(tee -a "$out/run.log") 2>&1
tee_pid=$!
scrub() {
  local rc=$?
  exec 1>&3 2>&4
  wait "$tee_pid" 2>/dev/null || true
  for f in "$out"/*; do
    [ -f "$f" ] || continue
    sed -i -e "s#$B#<build>#g" -e "s#$HOME#~#g" "$f"
  done
  rm -f "$out"/*.raw
  return "$rc"
}
trap scrub EXIT

RPC=http://127.0.0.1:3051
IDS="$CV_REPO/deploy/ids-local-pub.toml"
SEQ_LOG="$CV_RUN_DIR/local-pub.log"
# 1: after the native claim, also a token claim to a private holding (three proved transactions; README.md).
TOKEN="${CV_REAL_PROOF_TOKEN:-1}"
export LEE_WALLET_HOME_DIR="$CV_RUN_DIR/wallet-local-pub"
export LEE_WALLET_PASSWORD="${LEE_WALLET_PASSWORD:-}"
export RUST_LOG="${RUST_LOG:-warn}"
CV="$CARGO_TARGET_DIR/debug/cliff-veil"
TOTAL=10000000000
say() { printf '\n== %s  %s\n' "$(date -u +%FT%TZ)" "$*"; }
die() { echo "FAILED: $*" >&2; exit 1; }
# The CLI prints wallet sync lines before its JSON object.
json_of() { sed -n '/^{/,$p'; }
cv() { echo "\$ cliff-veil $*" >&2; "$CV" --ids "$IDS" "$@"; }
new_public() { wallet account new public --label "$1" | grep -oE 'Public/[1-9A-HJ-NP-Za-km-z]{32,44}' | head -1; }
# `wallet auth-transfer send` prints the transaction in full; keep the hash and inclusion lines.
transfer() { # to amount -> "hash block"
  local o
  o="$(wallet auth-transfer send --from cv-payer --to "$1" --amount "$2" | grep -v '^Transaction data is')"
  echo "$o" >&2
  echo "$(sed -n 's/^Transaction hash is \([0-9a-f]\{64\}\).*/\1/p' <<<"$o" | head -1) $(sed -n 's/^Transaction is included in block \([0-9]*\).*/\1/p' <<<"$o" | head -1)"
}
public_balance() { # Public/<id> -> balance or empty
  local o; o="$(wallet account get --account-id "$1" 2>&1 || true)"
  echo "$o" >&2
  grep -oiE 'balance"?[ :]+"?[0-9]+' <<<"$o" | grep -oE '[0-9]+$' | head -1
}
txs="$out/transactions.jsonl"; : > "$txs"
record_tx() { # step hash block
  jq -nc --arg step "$1" --arg hash "$2" --arg block "$3" '{step: $step, hash: $hash, block: ($block | tonumber? // null)}' >> "$txs"
}
sent_of() { jq -r '.sent[0] | "\(.hash) \(.block)"' "$1"; }
# Every 5 s: available memory, swap in use, RSS of r0vm (the prover) and of the client process <name> (its name as ps
# shows it: at most 15 characters).
start_sampler() { # file name
  printf 'utc\tmem_available_kib\tswap_used_kib\tr0vm_rss_kib\tclient_rss_kib\n' > "$1"
  (
    while :; do
      printf '%s\t%s\t%s\t%s\t%s\n' "$(date -u +%FT%TZ)" \
        "$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)" \
        "$(awk '/^SwapTotal:/{t=$2} /^SwapFree:/{f=$2} END{print t-f}' /proc/meminfo)" \
        "$(ps -C r0vm -o rss= | awk '{s+=$1} END{print s+0}')" \
        "$(ps -C "$2" -o rss= | awk '{s+=$1} END{print s+0}')"
      sleep 5
    done
  ) >> "$1" &
  sampler=$!
}
stop_sampler() { kill "$sampler" 2>/dev/null || true; wait "$sampler" 2>/dev/null || true; }
# Peaks of a sampler file: column -> maximum; available memory -> minimum; seconds with r0vm running.
peak() { awk -F'\t' -v c="$2" 'NR>1 && $c+0 > m {m=$c+0} END{print m+0}' "$1"; }
min_avail_of() { awk -F'\t' 'NR>1 && (m=="" || $2+0 < m) {m=$2+0} END{print m+0}' "$1"; }
r0vm_seconds_of() { awk -F'\t' 'NR>1 && $4+0 > 0 {n++} END{print (n+0)*5}' "$1"; }
time_field() { sed -n "s/^[[:space:]]*$2: //p" "$1" | head -1; }
rpc() { curl -fsS -m 30 -H 'Content-Type: application/json' -d "$1" "$RPC"; }
# Bytes of a transaction as the RPC carries it (base64 of its borsh encoding, proof included).
tx_bytes_of() {
  python3 -I -c '
import base64, re, sys
t = open(sys.argv[1]).read()
print(max((len(base64.b64decode(s)) for s in re.findall(r"\"([A-Za-z0-9+/=]{64,})\"", t)), default=0))
' "$1"
}

# ------------------------------------------------------------------ preconditions
say "preconditions: proofs are real and the sequencer verifies them"
[ -z "${RISC0_DEV_MODE:-}" ] || die "RISC0_DEV_MODE is set"
[ -f "$IDS" ] || die "no $IDS (scripts/deploy_local.sh pub)"
grep -qx 'dev_mode = false' "$IDS" || die "$IDS was not written for a sequencer without dev mode"
"$here/../../scripts/seq.sh" pub status
seq_pid=""
if [ -f "$CV_RUN_DIR/local-pub.pid" ]; then seq_pid="$(cat "$CV_RUN_DIR/local-pub.pid")"
else seq_pid="$(systemctl --user show -p MainPID --value "${CV_SEQ_UNIT_PUB:-cv-local-pub}" 2>/dev/null || true)"; fi
[ -n "$seq_pid" ] && [ -r "/proc/$seq_pid/environ" ] || die "cannot read the sequencer's environment (pid '$seq_pid')"
seq_dev_vars="$(tr '\0' '\n' < "/proc/$seq_pid/environ" | grep -c '^RISC0_DEV_MODE=' || true)"
echo "sequencer pid $seq_pid: RISC0_DEV_MODE entries in its environment: $seq_dev_vars"
[ "$seq_dev_vars" = 0 ] || die "the sequencer runs with RISC0_DEV_MODE in its environment"
command -v r0vm >/dev/null || die "r0vm not on PATH (scripts/bootstrap.sh)"
r0vm_version="$(r0vm --version 2>&1 | head -1)"
echo "prover: $r0vm_version"
[ -x /usr/bin/time ] || die "GNU time missing at /usr/bin/time"

say "machine"
cpu_model="$(sed -n 's/^model name[[:space:]]*: //p' /proc/cpuinfo | head -1)"
mem_total_kib="$(awk '/^MemTotal:/{print $2}' /proc/meminfo)"
swap_total_kib="$(awk '/^SwapTotal:/{print $2}' /proc/meminfo)"
echo "cpus $(nproc) ($cpu_model), memory $((mem_total_kib / 1024)) MiB, swap $((swap_total_kib / 1024)) MiB"
echo "runner image ${ImageOS:-unknown} ${ImageVersion:-unknown}"

say "CLI build"
(cd "$CV_REPO" && RISC0_SKIP_BUILD=1 cargo build --locked -q -p vesting_cli)
"$CV" --version

# ------------------------------------------------------------------ accounts and schedule
RUN="rp-$(date -u +%Y%m%dT%H%M%S)"
say "accounts: fresh public creator and beneficiary, funded from the local genesis test account cv-payer"
C="$(new_public "$RUN-creator")"; A="$(new_public "$RUN-beneficiary")"
[ -n "$C" ] && [ -n "$A" ] || die "could not create the accounts"
echo "creator $C"; echo "beneficiary $A"
read -r h b < <(transfer "$C" $((TOTAL * 2))); record_tx fund-creator "$h" "$b"
read -r h b < <(transfer "$A" 100000000); record_tx fund-beneficiary "$h" "$b"

say "create: native linear schedule, total $TOTAL, cliff after 30 s, end after 3 h (part of it still locked at the cancel)"
cv --json create --from "$RUN-creator" --beneficiary "$RUN-beneficiary" --total "$TOTAL" --start now --cliff +30s --end +3h \
  --label "$RUN" | json_of | tee "$out/create.json"
read -r h b < <(sent_of "$out/create.json"); record_tx create "$h" "$b"
S="$(cv address --creator "$RUN-creator" --label "$RUN")"
echo "schedule $S"
cv show "$S"

say "waiting for the cliff"
for _ in $(seq 1 40); do
  c="$(cv --json claimable "$S" | json_of | jq -r '.. | objects | select(has("claimable_now")) | .claimable_now' | head -1)"
  echo "claimable now: ${c:-?}"
  [ -n "$c" ] && [ "$c" != 0 ] && break
  sleep 15
done
[ -n "$c" ] && [ "$c" != 0 ] || die "nothing became claimable"

# ------------------------------------------------------------------ control: a dev-mode receipt is refused
say "control: the same private claim with a dev-mode receipt (RISC0_DEV_MODE=1) must be refused by this sequencer"
set +e
RISC0_DEV_MODE=1 "$CV" --ids "$IDS" --json claim "$S" --as "$RUN-beneficiary" --private --yes \
  > "$out/control.raw" 2> "$out/control.err.raw"
control_exit=$?
set -e
cat "$out/control.err.raw" "$out/control.raw"
json_of < "$out/control.raw" > "$out/control-dev-receipt.json" || true
jq -e . "$out/control-dev-receipt.json" >/dev/null 2>&1 || echo null > "$out/control-dev-receipt.json"
echo "[exit $control_exit]"
if [ "$control_exit" = 0 ]; then
  die "the sequencer accepted a dev-mode receipt: it does not verify proofs, so this run would prove nothing"
fi
# The client sees only that the transaction was never included; the sequencer logs why. The control counts only
# when the sequencer's own log names this transaction and an invalid proof as the reason.
control_hash="$( { jq -r '.error.message // empty' "$out/control-dev-receipt.json"; cat "$out/control.err.raw"; } \
  | grep -oE '[0-9a-f]{64}' | head -1 || true)"
[ -n "$control_hash" ] || die "the control's answer names no transaction hash (was it sent?)"
control_line=""
for _ in $(seq 1 8); do
  control_line="$(grep -F "$control_hash" "$SEQ_LOG" | grep -iE 'failed|invalid|reject' | head -1 || true)"
  [ -n "$control_line" ] && break
  sleep 15
done
echo "control transaction $control_hash; sequencer log: ${control_line:-<nothing>}"
[ -n "$control_line" ] || die "the sequencer log has no rejection line for the control transaction $control_hash"
grep -qi 'proof' <<<"$control_line" || die "the control was rejected for another reason than its proof: $control_line"
printf '%s\n' "$control_line" > "$out/control-sequencer-log.txt"

# ------------------------------------------------------------------ the proved private claim
say "private claim (ClaimAsOf) to a fresh private account of this wallet, RISC0_DEV_MODE unset: the proof is real"
mem="$out/memory.tsv"
start_sampler "$mem" cliff-veil
started="$(date -u +%FT%TZ)"; t0=$(date +%s)
echo "\$ cliff-veil --json claim $S --as $RUN-beneficiary --private --yes   (under /usr/bin/time -v)"
set +e
/usr/bin/time -v -o "$out/private-claim.time" "$CV" --ids "$IDS" --json claim "$S" --as "$RUN-beneficiary" --private --yes \
  > "$out/private-claim.raw" 2> "$out/private-claim.err.raw"
claim_exit=$?
set -e
t1=$(date +%s); finished="$(date -u +%FT%TZ)"
stop_sampler
cat "$out/private-claim.err.raw" "$out/private-claim.raw"; echo "[exit $claim_exit]"
cat "$out/private-claim.time"
json_of < "$out/private-claim.raw" > "$out/private-claim.json" || true
[ "$claim_exit" = 0 ] || die "the private claim failed (exit $claim_exit)"
jq -e .sent "$out/private-claim.json" >/dev/null || die "no JSON answer from the private claim"
P="$out/private-claim.json"
[ "$(jq -r '.transactions[0].dev_mode' "$P")" = false ] || die "the CLI reports dev mode"
read -r PH PB < <(sent_of "$P"); record_tx private-claim "$PH" "$PB"
PA="$(jq -r '.transactions[0].destination.account' "$P")"
PO="$(jq -r '.transactions[0].destination.one_time_address' "$P")"
echo "private claim: tx $PH in block $PB; canonical private account $PA; one-time address $PO"

say "RPC answers for the proved claim, and the privacy scan"
rpc "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getTransaction\",\"params\":[\"$PH\"]}" > "$out/private-claim-transaction.json"
rpc "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getBlock\",\"params\":[$PB]}" > "$out/private-claim-block.json"
python3 -I "$CV_REPO/tools/scan_rpc/scan_rpc.py" "$RPC" "$PB" "$PH" "$PA" "$PO" | tee "$out/scan.txt"
canonical_total="$(sed -n "s/^  RPC_TOTAL $PA //p" "$out/scan.txt")"
one_time_total="$(sed -n "s/^  RPC_TOTAL $PO //p" "$out/scan.txt")"
[ "$canonical_total" = 0 ] || die "the canonical private account appears in the RPC answers ($canonical_total)"
tx_bytes="$(tx_bytes_of "$out/private-claim-transaction.json")"
echo "transaction size: $tx_bytes bytes"
cv show "$S"

# ------------------------------------------------------------------ public claim and cancel
say "public claim of what vested meanwhile"
cv --json claim "$S" --as "$RUN-beneficiary" | json_of | tee "$out/public-claim.json"
read -r h b < <(sent_of "$out/public-claim.json"); record_tx public-claim "$h" "$b"

say "cancel: the unvested part goes back to the creator"
cv --json cancel "$S" --as "$RUN-creator" | json_of | tee "$out/cancel.json"
read -r h b < <(sent_of "$out/cancel.json"); record_tx cancel "$h" "$b"

say "final state"
cv --json show "$S" | json_of | tee "$out/schedule-final.json"
creator_balance="$(public_balance "$C")"
beneficiary_balance="$(public_balance "$A")"
wallet account sync-private >/dev/null 2>&1 || true
private_after="$(jq -r '.private_balance_after.native // empty' "$P")"

# ------------------------------------------------------------------ summary
max_rss_kib="$(time_field "$out/private-claim.time" 'Maximum resident set size (kbytes)')"
min_avail="$(min_avail_of "$mem")"
r0vm_seconds="$(r0vm_seconds_of "$mem")"
jq -n \
  --arg started "$started" --arg finished "$finished" \
  --argjson wall $((t1 - t0)) --arg elapsed "$(time_field "$out/private-claim.time" 'Elapsed (wall clock) time (h:mm:ss or m:ss)')" \
  --argjson max_rss "${max_rss_kib:-0}" --argjson r0vm_peak "$(peak "$mem" 4)" --argjson cli_peak "$(peak "$mem" 5)" \
  --argjson swap_peak "$(peak "$mem" 3)" --argjson min_avail "$min_avail" --argjson r0vm_seconds "$r0vm_seconds" \
  --argjson tx_bytes "$tx_bytes" --argjson canonical "$canonical_total" --argjson one_time "${one_time_total:-0}" \
  --arg schedule "$S" --arg creator "$C" --arg beneficiary "$A" --arg run "$RUN" \
  --arg pa "$PA" --arg po "$PO" --arg private_after "$private_after" \
  --arg creator_balance "$creator_balance" --arg beneficiary_balance "$beneficiary_balance" \
  --arg r0vm "$r0vm_version" --arg cpu "$cpu_model" --argjson cpus "$(nproc)" \
  --argjson mem_total "$mem_total_kib" --argjson swap_total "$swap_total_kib" \
  --arg image "${ImageOS:-unknown} ${ImageVersion:-unknown}" \
  --arg commit "$(git -C "$CV_REPO" rev-parse HEAD)" --arg lez "$(git -C "$CV_LEZ_DIR" rev-parse HEAD)" \
  --arg run_url "${GITHUB_SERVER_URL:-}${GITHUB_REPOSITORY:+/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID}" \
  --argjson control_exit "$control_exit" --arg seq_pid "$seq_pid" \
  --arg control_hash "$control_hash" --arg control_line "$control_line" \
  --slurpfile txs "$txs" --slurpfile private "$P" --slurpfile control "$out/control-dev-receipt.json" \
  --slurpfile public "$out/public-claim.json" --slurpfile cancel "$out/cancel.json" \
  --slurpfile final "$out/schedule-final.json" \
  --rawfile programs <(sed -n '/^\[programs\]/,/^\[build\]/p' "$IDS") \
  '{
    what: "private claim (ClaimAsOf) with a real RISC Zero proof on a standalone LEZ sequencer that verifies proofs",
    repo_commit: $commit, lez: "v0.4.0-rc3", lez_commit: $lez, run_url: $run_url, run: $run,
    machine: {cpus: $cpus, cpu: $cpu, mem_total_mib: ($mem_total / 1024 | floor), swap_total_mib: ($swap_total / 1024 | floor),
              image: $image, prover: $r0vm},
    sequencer: {rpc: "http://127.0.0.1:3051", risc0_dev_mode_in_its_environment: false, pid: $seq_pid,
                ephemeral: "started for this run on the same machine and discarded with it"},
    programs_toml: $programs,
    accounts: {creator: $creator, beneficiary: $beneficiary, schedule: $schedule},
    transactions: $txs,
    control_dev_mode_receipt: {refused: ($control_exit != 0), cli_exit: $control_exit, tx_hash: $control_hash,
                               sequencer_log_line: $control_line,
                               reason: ($control_line | capture("failed settlement: (?<r>.*)$").r? // null),
                               answer: ($control[0] // null)},
    private_claim: {
      hash: ($private[0].sent[0].hash), block: ($private[0].sent[0].block),
      amount: ($private[0].transactions[0].amount), dev_mode: ($private[0].transactions[0].dev_mode),
      canonical_private_account: $pa, one_time_address: $po, private_balance_after: $private_after,
      network_fee_estimate: ($private[0].transactions[0].estimate.network_fee), cycles: ($private[0].transactions[0].estimate.cycles),
      started: $started, finished: $finished, wall_seconds: $wall, elapsed_gnu_time: $elapsed,
      max_rss_kib_gnu_time: $max_rss, max_rss_gib_gnu_time: ($max_rss / 1048576 * 100 | round / 100),
      sampled: {r0vm_peak_rss_kib: $r0vm_peak, cli_peak_rss_kib: $cli_peak, swap_used_peak_kib: $swap_peak,
                mem_available_min_kib: $min_avail, r0vm_seen_seconds: $r0vm_seconds, interval_seconds: 5},
      transaction_bytes: $tx_bytes,
      rpc_scan: {canonical_account_total: $canonical, one_time_address_total: $one_time,
                 pass: ($canonical == 0)}
    },
    public_claim: {hash: ($public[0].sent[0].hash), block: ($public[0].sent[0].block), amount: ($public[0].transactions[0].amount)},
    cancel: {hash: ($cancel[0].sent[0].hash), block: ($cancel[0].sent[0].block), outcome: ($cancel[0].transactions[0])},
    final_schedule: ($final[0].schedule // $final[0]),
    balances_after: {creator_public: $creator_balance, beneficiary_public: $beneficiary_balance,
                     private_destination: $private_after}
  }' > "$out/summary.json"

say "native claim recorded in summary.json"

# ------------------------------------------------------------------ token claim to a private holding
# Runs after the native record is written, so a failure here cannot hide it. e2e/examples/real_proof_token.rs does
# the steps; it prints `rp` lines (step, value, error) that become token_private_claim in summary.json.
token_part() {
  (cd "$CV_REPO" && RISC0_SKIP_BUILD=1 cargo build --locked -q -p vesting_e2e --example real_proof_token)
  local bin="$CARGO_TARGET_DIR/debug/examples/real_proof_token" tmem="$out/memory-token.tsv" t0 t1 rc
  # The process name as ps shows it (15 characters).
  start_sampler "$tmem" real_proof_toke
  t0=$(date +%s)
  echo "\$ real_proof_token   (under /usr/bin/time -v; CV_SEQ_RPC=$RPC, CV_IDS=deploy/ids-local-pub.toml)"
  set +e
  CV_SEQ_RPC="$RPC" CV_IDS="$IDS" SUPPRESS_VERBOSE_PRINTS=1 /usr/bin/time -v -o "$out/token.time" "$bin" \
    > "$out/token.raw" 2> "$out/token.err.raw"
  rc=$?
  set -e
  t1=$(date +%s)
  stop_sampler
  cat "$out/token.err.raw" "$out/token.raw"; echo "[exit $rc]"
  cat "$out/token.time"
  { grep '^rp' "$out/token.raw" || true; } | jq -R -s '
    [split("\n")[] | select(length > 0) | split("\t")] as $l
    | {steps: [$l[] | select(.[1] == "step") | {step: .[2], hash: .[3], block: (.[4] | tonumber), seconds: (.[5] | tonumber)}],
       values: ([$l[] | select(.[1] == "value") | {(.[2]): .[3]}] | add // {}),
       error: ([$l[] | select(.[1] == "error") | .[2]][0] // null)}' > "$out/token-steps.json"
  local max_rss
  max_rss="$(time_field "$out/token.time" 'Maximum resident set size (kbytes)' 2>/dev/null || true)"
  jq -n --slurpfile r "$out/token-steps.json" --argjson exit "$rc" --argjson wall $((t1 - t0)) \
    --argjson max_rss "${max_rss:-0}" \
    --argjson r0vm_peak "$(peak "$tmem" 4)" --argjson client_peak "$(peak "$tmem" 5)" --argjson swap_peak "$(peak "$tmem" 3)" \
    --argjson min_avail "$(min_avail_of "$tmem")" --argjson r0vm_seconds "$(r0vm_seconds_of "$tmem")" \
    '$r[0] + {exit: $exit, wall_seconds_all_steps: $wall,
              max_rss_kib_gnu_time: $max_rss, max_rss_gib_gnu_time: ($max_rss / 1048576 * 100 | round / 100),
              sampled: {r0vm_peak_rss_kib: $r0vm_peak, client_peak_rss_kib: $client_peak, swap_used_peak_kib: $swap_peak,
                        mem_available_min_kib: $min_avail, r0vm_seen_seconds: $r0vm_seconds, interval_seconds: 5}}' \
    > "$out/token-summary.json"
  [ "$rc" = 0 ] || die "the token run failed (exit $rc): $(jq -r '.error // "see run.log"' "$out/token-steps.json")"

  local th tb dest payer one_time
  th="$(jq -r '.steps[] | select(.step == "private-token-claim") | .hash' "$out/token-steps.json")"
  tb="$(jq -r '.steps[] | select(.step == "private-token-claim") | .block' "$out/token-steps.json")"
  dest="$(jq -r '.values.destination_private_account' "$out/token-steps.json")"
  payer="$(jq -r '.values.private_fee_payer' "$out/token-steps.json")"
  one_time="$(jq -r '.values.one_time_address' "$out/token-steps.json")"
  say "RPC answers for the proved token claim, and the privacy scan (destination, fee payer, one-time address)"
  rpc "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getTransaction\",\"params\":[\"$th\"]}" > "$out/token-claim-transaction.json"
  rpc "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getBlock\",\"params\":[$tb]}" > "$out/token-claim-block.json"
  python3 -I "$CV_REPO/tools/scan_rpc/scan_rpc.py" "$RPC" "$tb" "$th" "$dest" "$payer" "$one_time" | tee "$out/token-scan.txt"
  local dest_total payer_total one_total
  dest_total="$(sed -n "s/^  RPC_TOTAL $dest //p" "$out/token-scan.txt")"
  payer_total="$(sed -n "s/^  RPC_TOTAL $payer //p" "$out/token-scan.txt")"
  one_total="$(sed -n "s/^  RPC_TOTAL $one_time //p" "$out/token-scan.txt")"
  jq --argjson d "$dest_total" --argjson p "$payer_total" --argjson o "${one_total:-0}" \
    --argjson bytes "$(tx_bytes_of "$out/token-claim-transaction.json")" \
    '. + {claim_transaction_bytes: $bytes,
          rpc_scan: {destination_total: $d, fee_payer_total: $p, one_time_address_total: $o, pass: ($d == 0 and $p == 0)}}' \
    "$out/token-summary.json" > "$out/token-summary.json.tmp" && mv "$out/token-summary.json.tmp" "$out/token-summary.json"
  [ "$dest_total" = 0 ] || die "the destination private account appears in the RPC answers ($dest_total)"
  [ "$payer_total" = 0 ] || die "the private fee payer appears in the RPC answers ($payer_total)"
}

token_status=skipped
if [ "$TOKEN" = 1 ]; then
  say "token: a token claim to a private holding; fund a private fee payer, initialise the holding, claim (three proofs)"
  set +e
  ( set -e; token_part )
  rc=$?
  set -e
  if [ "$rc" = 0 ]; then token_status=ok; else token_status=failed; fi
  [ -f "$out/token-summary.json" ] || echo '{}' > "$out/token-summary.json"
  jq --slurpfile t "$out/token-summary.json" --arg status "$token_status" \
    '. + {token_private_claim: ($t[0] + {status: $status})}' "$out/summary.json" > "$out/summary.json.tmp"
  mv "$out/summary.json.tmp" "$out/summary.json"
fi

say "done: summary.json (token part: $token_status)"
cat "$out/summary.json"
[ "$token_status" != failed ] || die "the native claim is recorded; the token part failed (token_private_claim in summary.json)"
