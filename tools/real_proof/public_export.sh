#!/usr/bin/env bash
# Private claim on the public chain, prover side (a GitHub-hosted runner; tools/real_proof/README.md): builds and
# proves the claim against the public read-only endpoint and keeps the signed, proven transaction instead of sending
# it. The sequencer's host submits it afterwards (tools/real_proof/submit_exported.sh).
#
#   tools/real_proof/public_export.sh init  <out dir>   a fresh wallet with one public account, the beneficiary;
#                                                       writes <out dir>/beneficiary.txt (its public id only)
#   tools/real_proof/public_export.sh claim <out dir>   waits until the schedule of tools/real_proof/public-claim.env
#                                                       exists for that beneficiary and has something claimable,
#                                                       then makes the private claim with RISC0_DEV_MODE unset
#
# The wallet lives in CV_EXPORT_WALLET (default $RUNNER_TEMP/cv-export-wallet, else a directory under $TMPDIR), never
# under <out dir>: it holds the beneficiary's key, which stays on the machine that signs. The wallet and the CLI talk
# to tools/real_proof/export_proxy.py on 127.0.0.1:$CV_EXPORT_PORT (default 8547), which forwards reads to
# CV_PUBLIC_URL and writes the `sendTransaction` request to <out dir>/proxy/send-1.json instead of sending it.
#
# <out dir> after `claim`: exported-tx.json (that request, byte for byte) and its sha256, claim.json (the CLI's JSON
# answer: amount, one-time address, fee, dev_mode), claim.time (/usr/bin/time -v), memory.tsv, proxy/methods.tsv
# (every RPC call and its outcome), export-summary.json. Nothing in it is secret.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/../../scripts/env.sh"
# env.sh turns dev mode on for local work; the claim is proved for real unless CV_EXPORT_DEV_MODE=1 (a rehearsal).
unset RISC0_DEV_MODE
[ "${CV_EXPORT_DEV_MODE:-0}" = 1 ] && export RISC0_DEV_MODE=1
set -a
# shellcheck disable=SC1091
source "$here/public-claim.env"
set +a
# Overrides for a rehearsal (another endpoint, another schedule label).
CV_PUBLIC_URL="${CV_EXPORT_UPSTREAM:-$CV_PUBLIC_URL}"
CV_PUBLIC_LABEL="${CV_EXPORT_LABEL:-$CV_PUBLIC_LABEL}"

phase="${1:?usage: $0 init|claim <out dir>}"
out="${2:?usage: $0 init|claim <out dir>}"
mkdir -p "$out"; out="$(cd "$out" && pwd)"
wallet_dir="${CV_EXPORT_WALLET:-${RUNNER_TEMP:-$TMPDIR}/cv-export-wallet}"
port="${CV_EXPORT_PORT:-8547}"
export LEE_WALLET_HOME_DIR="$wallet_dir" LEE_WALLET_PASSWORD="${LEE_WALLET_PASSWORD:-}" RUST_LOG="${RUST_LOG:-warn}"
CV="$CARGO_TARGET_DIR/debug/cliff-veil"
say() { printf '\n== %s  %s\n' "$(date -u +%FT%TZ)" "$*"; }
die() { echo "FAILED: $*" >&2; exit 1; }
json_of() { sed -n '/^{/,$p'; }
case "$out" in "$wallet_dir"|"$wallet_dir"/*) die "the out dir must not hold the wallet" ;; esac

start_proxy() {
  python3 -I "$here/export_proxy.py" "$port" "$CV_PUBLIC_URL" "$out/proxy" >> "$out/proxy.log" 2>&1 &
  proxy=$!
  for _ in $(seq 1 20); do
    curl -fsS -m 2 -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getLastBlockId","params":[]}' \
      "http://127.0.0.1:$port/" >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  die "the export proxy did not start (see $out/proxy.log)"
}
stop_proxy() { kill "${proxy:-}" 2>/dev/null || true; }
trap stop_proxy EXIT

case "$phase" in
  init)
    [ ! -e "$wallet_dir/storage.json" ] || die "$wallet_dir already holds a wallet"
    mkdir -p "$wallet_dir"; chmod 700 "$wallet_dir"
    # LEZ's own wallet configuration, pointed at the proxy; block ranges of 16 (the public endpoint's limit).
    jq --arg url "http://127.0.0.1:$port" '.sequencer.sequencer_addr = $url | .seq_block_poll_max_amount = 16' \
      "$CV_LEZ_DIR/lez/wallet/configs/debug/wallet_config.json" > "$wallet_dir/wallet_config.json"
    start_proxy
    # The first command of a new wallet prints its recovery phrase on stdout: keep only the id and public key lines.
    made="$(wallet account new public --label beneficiary | grep -E '^(Generated new account|With pk) ')"
    id="$(grep -oE 'Public/[1-9A-HJ-NP-Za-km-z]{32,44}' <<< "$made" | head -1)"
    pk="$(sed -n 's/^With pk \([0-9a-f]\{64\}\)$/\1/p' <<< "$made")"
    [ -n "$id" ] && [ -n "$pk" ] || die "could not create the beneficiary account"
    echo "${id#Public/}" > "$out/beneficiary.txt"
    echo "$pk" > "$out/beneficiary-pk.txt"
    say "beneficiary (public id and public key; its private key stays in this machine's wallet): ${id#Public/} $pk"
    ;;
  claim)
    [ -f "$out/beneficiary.txt" ] && [ -f "$wallet_dir/storage.json" ] || die "run init first"
    ben="$(cat "$out/beneficiary.txt")"
    if [ -z "${RISC0_DEV_MODE:-}" ]; then
      command -v r0vm >/dev/null || die "r0vm not on PATH (scripts/bootstrap.sh)"
      r0vm --version
    fi
    start_proxy
    S="$("$CV" address --creator "$CV_PUBLIC_CREATOR" --label "$CV_PUBLIC_LABEL")"
    say "schedule $S (creator $CV_PUBLIC_CREATOR, label $CV_PUBLIC_LABEL); waiting until it exists for $ben with something claimable"
    deadline=$(( $(date +%s) + 60 * ${CV_EXPORT_WAIT_MIN:-150} ))
    while :; do
      doc="$("$CV" --json show "$S" 2>/dev/null | json_of || true)"
      b="$(jq -r '.schedule.beneficiary // empty' <<<"$doc" 2>/dev/null || true)"
      c="$(jq -r '.schedule.claimable_now // empty' <<<"$doc" 2>/dev/null || true)"
      if [ -n "$b" ]; then
        [ "$b" = "$ben" ] || die "schedule $S names beneficiary $b, not $ben"
        [ -n "$c" ] && [ "$c" != 0 ] && break
      fi
      [ "$(date +%s)" -lt "$deadline" ] || die "no claimable schedule $S for $ben in time"
      echo "$(date -u +%FT%TZ) beneficiary ${b:-<no schedule yet>} claimable ${c:-?}"
      sleep 30
    done
    "$CV" show "$S"

    say "private claim (ClaimAsOf) to a fresh private account, RISC0_DEV_MODE=${RISC0_DEV_MODE:-unset}; the proxy keeps the transaction"
    printf 'utc\tmem_available_kib\tswap_used_kib\tr0vm_rss_kib\n' > "$out/memory.tsv"
    (
      while :; do
        printf '%s\t%s\t%s\t%s\n' "$(date -u +%FT%TZ)" "$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)" \
          "$(awk '/^SwapTotal:/{t=$2} /^SwapFree:/{f=$2} END{print t-f}' /proc/meminfo)" \
          "$(ps -C r0vm -o rss= | awk '{s+=$1} END{print s+0}')"
        sleep 5
      done
    ) >> "$out/memory.tsv" &
    sampler=$!
    t0=$(date +%s); started="$(date -u +%FT%TZ)"
    set +e
    /usr/bin/time -v -o "$out/claim.time" "$CV" --json claim "$S" --as beneficiary --private --yes \
      > "$out/claim.stdout" 2> "$out/claim.stderr"
    rc=$?
    set -e
    t1=$(date +%s)
    kill "$sampler" 2>/dev/null || true; wait "$sampler" 2>/dev/null || true
    cat "$out/claim.stderr" "$out/claim.stdout"; echo "[exit $rc]"; cat "$out/claim.time"
    json_of < "$out/claim.stdout" > "$out/claim.json" || true
    rm -f "$out/claim.stdout" "$out/claim.stderr"
    # The CLI reports the kept transaction as a failed send (the proxy's answer); the request file is the result.
    [ -f "$out/proxy/send-1.json" ] || die "no transaction was kept (exit $rc; see claim.json and proxy/methods.tsv)"
    [ ! -f "$out/proxy/send-2.json" ] || die "more than one transaction was kept"
    cp "$out/proxy/send-1.json" "$out/exported-tx.json"
    sha256sum "$out/exported-tx.json" | cut -c1-64 > "$out/exported-tx.sha256"
    jq -e '.transactions[0].destination.kind == "private"' "$out/claim.json" >/dev/null || die "no private claim in claim.json"
    refused="$(awk -F'\t' '$3 ~ /^refused upstream/ {print $2}' "$out/proxy/methods.tsv" | sort -u | tr '\n' ' ')"
    jq -n --slurpfile c "$out/claim.json" --arg ben "$ben" --arg schedule "$S" --arg started "$started" \
      --argjson wall $((t1 - t0)) --arg sha "$(cat "$out/exported-tx.sha256")" --argjson bytes "$(stat -c %s "$out/exported-tx.json")" \
      --arg max_rss "$(sed -n 's/^[[:space:]]*Maximum resident set size (kbytes): //p' "$out/claim.time")" \
      --arg r0vm_peak "$(awk -F'\t' 'NR>1 && $4+0 > m {m=$4+0} END{print m+0}' "$out/memory.tsv")" \
      --arg refused "$refused" --arg dev "${RISC0_DEV_MODE:-}" --arg url "$CV_PUBLIC_URL" \
      '{what: "private claim built and proved against the public read-only endpoint; the signed, proven transaction kept, not sent",
        endpoint: $url, beneficiary: $ben, schedule: $schedule, risc0_dev_mode: ($dev != ""),
        cli_dev_mode: $c[0].transactions[0].dev_mode, amount: $c[0].transactions[0].amount,
        as_of: $c[0].transactions[0].at, canonical_private_account: $c[0].transactions[0].destination.account,
        one_time_address: $c[0].transactions[0].destination.one_time_address,
        network_fee_estimate: $c[0].transactions[0].estimate.network_fee,
        started: $started, wall_seconds: $wall, max_rss_kib_gnu_time: ($max_rss | tonumber? // null),
        r0vm_peak_rss_kib: ($r0vm_peak | tonumber), exported_tx_sha256: $sha, exported_request_bytes: $bytes,
        methods_refused_upstream: ($refused | split(" ") | map(select(length > 0)))}' > "$out/export-summary.json"
    cat "$out/export-summary.json"
    ;;
  *) die "usage: $0 init|claim <out dir>" ;;
esac
