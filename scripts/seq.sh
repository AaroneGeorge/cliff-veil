#!/usr/bin/env bash
# Local LEZ v0.4.0-rc3 standalone sequencers (mock Bedrock, 15 s blocks, LEZ's local genesis).
#
#   scripts/seq.sh dev|pub start|stop|status|reset|logs|wallet-check
#
#   dev : network local-dev, RISC0_DEV_MODE=1, RPC 127.0.0.1:3050, metrics 127.0.0.1:9050, home $CV_RUN_DIR/local-dev
#   pub : network local-pub, dev mode UNSET,   RPC 127.0.0.1:3051, metrics 127.0.0.1:9051, home $CV_RUN_DIR/local-pub
#         (public transactions are executed by the sequencer itself; gas is charged as on a network)
#
#   Wallet home per sequencer: $CV_RUN_DIR/wallet-<name>, its config pointed at that sequencer's RPC.
#   Log: $CV_RUN_DIR/<name>.log. Ports can be moved with CV_SEQ_RPC_PORT / CV_SEQ_METRICS_PORT.
#
# Detached runner: where a systemd user manager is available, the sequencer runs as the transient user unit
# cv-<name> (CV_SEQ_UNIT_DEV / CV_SEQ_UNIT_PUB override the name) with MemoryMax=$CV_SEQ_MEMORY_MAX (default 1500M),
# so it outlives the shell that started it and cannot take the machine's memory. Elsewhere (CI runners) it runs under setsid with a pid file. CV_SEQ_RUNNER=setsid
# forces the second form.
#
# A long chain takes a long time to replay on restart: do not restart a sequencer in the middle of a recorded run.
# Binary: $CV_LEZ_BIN/sequencer_service built with `--features standalone` (scripts/bootstrap.sh). The config
# template and the funding key are LEZ's own (Justfile `run-sequencer-standalone`), read from $CV_LEZ_DIR.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/env.sh"

usage() { echo "usage: $0 dev|pub start|stop|status|reset|logs|wallet-check" >&2; exit 2; }
which_seq="${1:-}"; cmd="${2:-status}"
case "$which_seq" in
  dev) name=local-dev; port=3050; ops=9050 ;;
  pub) name=local-pub; port=3051; ops=9051 ;;
  *) usage ;;
esac
port="${CV_SEQ_RPC_PORT:-$port}"; ops="${CV_SEQ_METRICS_PORT:-$ops}"

SEQ_BIN="$CV_LEZ_BIN/sequencer_service"
home="$CV_RUN_DIR/$name"
wallet_home="$CV_RUN_DIR/wallet-$name"
log="$CV_RUN_DIR/$name.log"
pidf="$CV_RUN_DIR/$name.pid"
unit_var="CV_SEQ_UNIT_${which_seq^^}"
unit="${!unit_var:-cv-$name}"

# Funding key of LEZ's local Bedrock node: the Justfile's BEDROCK_FUNDING_PUBLIC_KEY, read at run time.
funding_pk() {
  sed -n 's/^BEDROCK_FUNDING_PUBLIC_KEY *:= *"\([0-9a-f]\{64\}\)".*/\1/p' "$CV_LEZ_DIR/Justfile" | head -1
}

runner="${CV_SEQ_RUNNER:-}"
if [ -z "$runner" ]; then
  export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  if command -v systemd-run >/dev/null && systemctl --user show-environment >/dev/null 2>&1; then runner=systemd
  else runner=setsid; fi
fi

running() {
  if [ "$runner" = systemd ]; then systemctl --user is-active --quiet "$unit"
  else [ -f "$pidf" ] && kill -0 "$(cat "$pidf")" 2>/dev/null; fi
}
healthy() { curl -fsS -m 3 "http://127.0.0.1:$ops/health" >/dev/null 2>&1; }

ensure_wallet_config() {
  mkdir -p "$wallet_home"
  if [ ! -f "$wallet_home/wallet_config.json" ]; then
    sed "s#http://127.0.0.1:3040#http://127.0.0.1:$port#" "$CV_LEZ_DIR/lez/wallet/configs/debug/wallet_config.json" \
      > "$wallet_home/wallet_config.json"
  fi
  grep -q "127.0.0.1:$port" "$wallet_home/wallet_config.json" \
    || { echo "$wallet_home/wallet_config.json does not point at port $port" >&2; exit 1; }
}

case "$cmd" in
  start)
    if running; then echo "$name already running ($runner)"; exit 0; fi
    [ -x "$SEQ_BIN" ] || { echo "missing $SEQ_BIN (scripts/bootstrap.sh builds it)" >&2; exit 1; }
    mkdir -p "$CV_RUN_DIR"
    if [ ! -f "$home/sequencer_config.json" ]; then
      pk="$(funding_pk)"; [ -n "$pk" ] || { echo "no BEDROCK_FUNDING_PUBLIC_KEY in $CV_LEZ_DIR/Justfile" >&2; exit 1; }
      "$SEQ_BIN" setup "$home" --config "$CV_LEZ_DIR/lez/sequencer/service/networks/local.json" \
        --funding-public-key "$pk" --create-channel
    fi
    ensure_wallet_config
    dev_mode=1; [ "$which_seq" = pub ] && dev_mode=""
    rust_log="${RUST_LOG:-info,risc0_zkvm=warn,kameo=warn}"
    args=(start "$home" --rpc-listen-address "127.0.0.1:$port" --metrics-listen-address "127.0.0.1:$ops")
    printf '\n=== %s start %s runner=%s RISC0_DEV_MODE=%s\n' "$(date -u +%FT%TZ)" "$name" "$runner" "${dev_mode:-unset}" >> "$log"
    if [ "$runner" = systemd ]; then
      systemctl --user reset-failed "$unit" >/dev/null 2>&1 || true
      env_args=(--setenv=RUST_LOG="$rust_log")
      if [ -n "$dev_mode" ]; then env_args+=(--setenv=RISC0_DEV_MODE=1); else env_args+=(-p UnsetEnvironment=RISC0_DEV_MODE); fi
      systemd-run --user --unit="$unit" --description="cliff-veil LEZ sequencer $name" --quiet \
        -p MemoryMax="${CV_SEQ_MEMORY_MAX:-1500M}" -p MemorySwapMax="${CV_SEQ_SWAP_MAX:-1G}" --nice=10 \
        -p WorkingDirectory="$home" -p StandardOutput=append:"$log" -p StandardError=append:"$log" \
        "${env_args[@]}" -- "$SEQ_BIN" "${args[@]}"
    else
      if [ -n "$dev_mode" ]; then export RISC0_DEV_MODE=1; else unset RISC0_DEV_MODE; fi
      RUST_LOG="$rust_log" setsid nohup nice -n 10 "$SEQ_BIN" "${args[@]}" >> "$log" 2>&1 < /dev/null &
      echo $! > "$pidf"
    fi
    for _ in $(seq 1 90); do
      if healthy; then
        echo "$name up ($runner${dev_mode:+, dev mode}): rpc http://127.0.0.1:$port, metrics 127.0.0.1:$ops, log $log"
        echo "wallet: export LEE_WALLET_HOME_DIR=$wallet_home"
        exit 0
      fi
      running || { echo "$name exited; tail of $log:" >&2; tail -20 "$log" >&2; exit 1; }
      sleep 1
    done
    echo "$name started but /health did not answer in 90 s; see $log" >&2
    exit 1
    ;;
  stop)
    if ! running; then echo "$name not running"; rm -f "$pidf"; exit 0; fi
    if [ "$runner" = systemd ]; then
      systemctl --user stop "$unit"
    else
      pid="$(cat "$pidf")"; kill -TERM "$pid"
      for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
      kill -0 "$pid" 2>/dev/null && kill -KILL "$pid"
    fi
    rm -f "$pidf"
    echo "$name stopped"
    ;;
  status)
    if running && healthy; then
      echo "$name running ($runner): rpc http://127.0.0.1:$port, health $(curl -fsS -m 3 "http://127.0.0.1:$ops/health")"
    elif running; then
      echo "$name running ($runner) but /health on 127.0.0.1:$ops does not answer"; exit 4
    else
      echo "$name stopped"; exit 3
    fi
    ;;
  wallet-check)
    # The wallet of this sequencer reaches it and LEZ's builtin programs match the remote ones.
    ensure_wallet_config
    LEE_WALLET_HOME_DIR="$wallet_home" LEE_WALLET_PASSWORD="${LEE_WALLET_PASSWORD:-}" "$CV_LEZ_BIN/wallet" check-health
    ;;
  reset)
    running && { echo "stop $name first" >&2; exit 1; }
    rm -rf "$home" "$wallet_home/storage.json"
    echo "$name state wiped (wallet config kept)"
    ;;
  logs) tail -n 50 -f "$log" ;;
  *) usage ;;
esac
