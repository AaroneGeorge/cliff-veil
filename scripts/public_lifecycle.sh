#!/usr/bin/env bash
# Recorded lifecycle on the public-chain sequencer local-pub (scripts/seq.sh pub: RISC0_DEV_MODE unset), with the
# cliff-veil command line only. Every transaction is public; the sequencer executes it, no proof is involved.
#
#   scripts/public_lifecycle.sh [--url PUBLIC_URL] [--deploy-log FILE]
#
#   --deploy-log FILE  the output of `scripts/deploy_local.sh pub`: kept as deploy.log (without the wallet's
#                      transaction dumps), so run.json also lists the deployment transactions
#
# Needs: local-pub running, programs deployed (scripts/deploy_local.sh pub --build release), the CLI built
# (cargo build -p vesting_cli). Writes evidence/public-chain/<utc>/: run.log (every command and its JSON output),
# steps/NN.json (one JSON document per command) and run.json (every transaction hash with its block and what it
# is, every refusal with its code, and the final state of every schedule; scripts/verify_public.sh reads it).
#
# Refusals are the SDK's: the command line prices and checks every operation before sending, so a refused
# operation is reported with its error code (SPEC 5) and nothing is sent.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/env.sh"
unset RISC0_DEV_MODE

url=""; deploy_log=""
while [ $# -gt 0 ]; do
  case "$1" in
    --url) url="$2"; shift 2 ;;
    --deploy-log) deploy_log="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

"$here/seq.sh" pub status >/dev/null || { echo "local-pub is not running: scripts/seq.sh pub start" >&2; exit 1; }
IDS="$CV_REPO/deploy/ids-local-pub.toml"
[ -f "$IDS" ] || { echo "no $IDS: scripts/deploy_local.sh pub --build release" >&2; exit 1; }
CV="${CV_CLI:-$CARGO_TARGET_DIR/debug/cliff-veil}"
[ -x "$CV" ] || { echo "no $CV: cargo build -p vesting_cli" >&2; exit 1; }
export LEE_WALLET_HOME_DIR="$CV_RUN_DIR/wallet-local-pub" LEE_WALLET_PASSWORD="${LEE_WALLET_PASSWORD:-}"
export RUST_LOG="${RUST_LOG:-warn}"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
out="$CV_REPO/evidence/public-chain/$stamp"
mkdir -p "$out/steps"
log="$out/run.log"
if [ -n "$deploy_log" ]; then
  grep -v '^Transaction data is' "$deploy_log" | sed "s#$B/#\$B/#g; s#$HOME/#~/#g" > "$out/deploy.log"
fi
index="$out/steps/index.tsv"
: > "$index"
run="pub-$stamp"
n=0

say() { echo "$*" | tee -a "$log"; }
# step EXPECT WHAT ARGS...: runs `cliff-veil --json --yes ARGS`, keeps its JSON, records what it is.
# EXPECT is ok (a transaction or a query is expected) or refused (the SDK must refuse it, nothing sent).
step() {
  local expect="$1" what="$2"; shift 2
  n=$((n + 1))
  local f; f="$(printf '%s/steps/%02d.json' "$out" "$n")"
  say ""
  say "[$n] $what"
  say "\$ cliff-veil $*"
  local rc=0
  "$CV" --ids "$IDS" --json --yes "$@" > "$f" 2>> "$log" || rc=$?
  cat "$f" >> "$log"
  say "[exit $rc]"
  printf '%02d\t%s\t%s\t%s\t%s\n' "$n" "$expect" "$rc" "$what" "$*" >> "$index"
  if [ "$expect" = ok ] && [ "$rc" != 0 ]; then echo "step $n failed: $what" >&2; exit 1; fi
  if [ "$expect" = refused ] && [ "$rc" = 0 ]; then echo "step $n was not refused: $what" >&2; exit 1; fi
}
new_account() { # label -> Public/<id>
  wallet account new public --label "$1" | grep -oE 'Public/[1-9A-HJ-NP-Za-km-z]{32,44}' | head -1
}
fund() { # to amount: a native transfer from the deployer's account; its hash and block go to the index
  local r h b
  r="$(wallet auth-transfer send --from cv-payer --to "$1" --amount "$2" | grep -E 'Transaction (hash|is included)')"
  h="$(sed -n 's/^Transaction hash is \([0-9a-f]\{64\}\)$/\1/p' <<< "$r")"
  b="$(sed -n 's/^Transaction is included in block \([0-9]*\)$/\1/p' <<< "$r")"
  [ -n "$h" ] && [ -n "$b" ] || { echo "funding $1 failed" >&2; exit 1; }
  say "fund $1 with $2: tx $h in block $b"
  printf 'fund\t%s\t%s\t%s\t%s\n' "$h" "$b" "$1" "$2" >> "$out/steps/funding.tsv"
}

say "# cliff-veil public lifecycle on local-pub (LEZ v0.4.0-rc3 standalone sequencer, RISC0_DEV_MODE unset)"
say "# $(date -u +%FT%TZ)  run $run  rpc $(jq -r .sequencer.sequencer_addr "$LEE_WALLET_HOME_DIR/wallet_config.json")"
say "# cliff-veil $("$CV" --version | awk '{print $2}'), repository commit $(git -C "$CV_REPO" rev-parse --short=12 HEAD)"
sed -n '/^\[programs\]/,$p' "$IDS" | tee -a "$log" >/dev/null

say ""
say "== accounts (fresh public accounts, funded from the deployer's account)"
CREATOR="$(new_account "$run-creator")"; ALICE="$(new_account "$run-alice")"; BOB="$(new_account "$run-bob")"
MALLORY="$(new_account "$run-mallory")"
say "creator $CREATOR"; say "alice $ALICE"; say "bob $BOB"; say "mallory $MALLORY (holds no role in any schedule)"
: > "$out/steps/funding.tsv"
fund "$CREATOR" 60000000000
for a in "$ALICE" "$BOB" "$MALLORY"; do fund "$a" 2000000000; done

# Batch recipients: 20 fresh accounts; the first two get fees to claim with.
cd "$CV_REPO"
csv="evidence/public-chain/$stamp/batch.csv"   # relative to the repository root, as it appears in the log
: > "$csv"
for i in $(seq -w 1 20); do
  r="$(new_account "$run-r$i")"
  echo "${r#Public/},$((100000000 + 10#$i * 1000000)),$run-b$i" >> "$csv"
done
R1="Public/$(sed -n 1p "$csv" | cut -d, -f1)"; R2="Public/$(sed -n 2p "$csv" | cut -d, -f1)"
fund "$R1" 500000000; fund "$R2" 500000000

say ""
say "== creation"
step ok "fee configuration (vesting_fees Init by the deployer)" fee-show
step ok "create: linear with a cliff (cliff +120 s, end +900 s), transferable, cancelable" \
  create --from "$run-creator" --beneficiary "$run-alice" --total 1000000000 --start now --cliff +120s --end +900s \
  --transferable --label "$run-cliff"
CLIFF="$("$CV" --ids "$IDS" address --creator "$run-creator" --label "$run-cliff")"
step ok "claimable now and next unlock (cliff schedule, before the cliff)" claimable "$CLIFF"
step refused "claim before the cliff (nothing claimable yet)" claim "$CLIFF" --as "$run-alice"
step ok "create: fully linear (no cliff, end +180 s)" \
  create --from "$run-creator" --beneficiary "$run-alice" --total 600000000 --start now --end +180s --label "$run-linear"
step ok "create: milestones 300000000 + 700000000, milestone authority the creator" \
  create --from "$run-creator" --beneficiary "$run-alice" --milestones 300000000,700000000 --label "$run-milestones"
step ok "create: linear schedule for the cancel with refund (end +600 s), refund to bob" \
  create --from "$run-creator" --beneficiary "$run-alice" --total 900000000 --start now --end +600s \
  --refund-to "$run-bob" --label "$run-cancel"
step ok "create-batch: 20 linear schedules with a cliff (cliff +60 s, end +900 s)" \
  create-batch --from "$run-creator" --file "$csv" --start now --cliff +60s --end +900s

LINEAR="$("$CV" --ids "$IDS" address --creator "$run-creator" --label "$run-linear")"
MILES="$("$CV" --ids "$IDS" address --creator "$run-creator" --label "$run-milestones")"
CANCEL="$("$CV" --ids "$IDS" address --creator "$run-creator" --label "$run-cancel")"
B1="$("$CV" --ids "$IDS" address --creator "$run-creator" --label "$run-b01")"
B2="$("$CV" --ids "$IDS" address --creator "$run-creator" --label "$run-b02")"
printf 'cliff\t%s\nlinear\t%s\nmilestones\t%s\ncancel\t%s\n' "$CLIFF" "$LINEAR" "$MILES" "$CANCEL" > "$out/steps/schedules.tsv"
for i in $(seq -w 1 20); do
  printf 'batch-%s\t%s\n' "$i" "$("$CV" --ids "$IDS" address --creator "$run-creator" --label "$run-b$i")" >> "$out/steps/schedules.tsv"
done
say "schedules: cliff $CLIFF, linear $LINEAR, milestones $MILES, cancel $CANCEL, batch #1 $B1, #2 $B2"

say ""
say "== queries and refusals before the cliff"
step ok "show the cliff schedule" show "$CLIFF"
step refused "claim by an account that is not the beneficiary" claim "$LINEAR" --as "$run-mallory"
step refused "transfer of a schedule created non-transferable" transfer "$LINEAR" --to "$run-bob" --as "$run-alice"
step refused "milestone signal by an account that is not the milestone authority" signal "$MILES" --index 0 --as "$run-mallory"
step refused "cancel by an account that is not the cancel authority" cancel "$CANCEL" --as "$run-mallory"
step refused "make-non-cancelable by an account that is not the creator" make-non-cancelable "$LINEAR" --as "$run-mallory"
step ok "list the schedules the creator made (block scan)" list --creator "$run-creator" --no-cache

say ""
say "== milestones"
step ok "signal milestone #0" signal "$MILES" --index 0 --as "$run-creator"
step refused "signal milestone #0 again" signal "$MILES" --index 0 --as "$run-creator"
step ok "public claim of the signalled milestone" claim "$MILES" --as "$run-alice"
step ok "show the milestone schedule" show "$MILES"

say ""
say "== linear claims"
step ok "public claim, fully linear schedule" claim "$LINEAR" --as "$run-alice"
step ok "make the linear schedule non-cancelable" make-non-cancelable "$LINEAR" --as "$run-creator"
step refused "cancel a non-cancelable schedule" cancel "$LINEAR" --as "$run-creator"

say ""
say "== waiting for the cliff"
for _ in $(seq 1 60); do
  c="$("$CV" --ids "$IDS" --json claimable "$CLIFF" | jq -r .claimable.claimable_now)"
  [ "$c" != 0 ] && break
  sleep 10
done
step ok "claimable now and next unlock (cliff schedule, after the cliff)" claimable "$CLIFF"
step ok "public claim after the cliff" claim "$CLIFF" --as "$run-alice"
step ok "public claim from batch schedule #1 by its recipient" claim "$B1" --as "$R1"
step ok "public claim from batch schedule #2 by its recipient" claim "$B2" --as "$R2"

say ""
say "== beneficiary transfer"
step ok "transfer the cliff schedule from alice to bob" transfer "$CLIFF" --to "$run-bob" --as "$run-alice"
step refused "claim by the previous beneficiary after the transfer" claim "$CLIFF" --as "$run-alice"
step ok "public claim by the new beneficiary" claim "$CLIFF" --as "$run-bob"

say ""
say "== cancel with refund"
step ok "cancel: the unvested part goes to the refund account (bob), the vested part stays claimable" \
  cancel "$CANCEL" --as "$run-creator"
step refused "cancel the same schedule again" cancel "$CANCEL" --as "$run-creator"
step ok "public claim of the vested part after the cancel" claim "$CANCEL" --as "$run-alice"

say ""
say "== final state"
for s in "$CLIFF" "$LINEAR" "$MILES" "$CANCEL" "$B1" "$B2"; do step ok "show $s" show "$s"; done
step ok "list what alice can claim now (block scan)" list --beneficiary "$run-alice" --no-cache

python3 -I "$here/public_run_json.py" "$out" ${url:+--url "$url"}
say ""
say "wrote $out/run.json"
