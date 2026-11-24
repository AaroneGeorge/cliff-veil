#!/usr/bin/env bash
# Deploys the three programs (immutable) to a local sequencer of scripts/seq.sh, in the order SPEC 1.1 and 6 need,
# and records every id in deploy/ids-<network>.toml.
#
#   scripts/deploy_local.sh [dev|pub] [--build local|release] [--commit REV] [--resume|--only-vesting] [--ids FILE]
#
#   1. token, vesting_fees and ata, built without build inputs, deployed. ata (program/ata-guest) is built only by the
#      release build; a local build deploys it when $CARGO_TARGET_DIR/guests/ata.bin exists, else skips it.
#   2. vesting_fees `Init`, sent by the deployer right after its deployment (signed by the program's header account,
#      whose key this wallet holds), so the admin slot is never open to anyone else (SPEC 6).
#   3. vesting, built with CV_TOKEN_PROGRAM_ID and CV_FEES_PROGRAM_ID (and CV_ATA_PROGRAM_ID when `ata` is in the ids
#      file) set to the ids just deployed, deployed.
#
#   --build local     the local rzup toolchain (`cargo build -p vesting_methods`, as `make guests`); default
#   --build release   scripts/guests_release.sh: the pinned image, from the committed tree of --commit (default HEAD);
#                     the image ids it records can be rebuilt by anyone (scripts/check_image_ids.sh <dir> <ids file>)
#   --resume          keep what the ids file already records (token, vesting_fees and its Init, ata) and do the
#                     remaining steps; vesting is always rebuilt and redeployed
#   --only-vesting    keep token, vesting_fees and ata of the ids file; rebuild and redeploy vesting only
#   --ids FILE        ids file, relative to the repo root unless absolute (default deploy/ids-<network>.toml)
#
# Networks: dev = local-dev (RPC 3050, dev mode), pub = local-pub (RPC 3051). The ids file is rewritten atomically
# (temp file + rename) under flock on FILE.lock, which also serialises wallet use between deploys.
#
# Payer: the wallet account labelled cv-payer. On a fresh wallet it is LEZ's local genesis test account, imported
# with the key the LEZ Justfile publishes for the local network (`wallet-import-test-accounts`), read at run time.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/env.sh"

which_seq=dev; build=local; rev=HEAD; only_vesting=0; resume=0; out=""
while [ $# -gt 0 ]; do
  case "$1" in
    dev|pub) which_seq="$1"; shift ;;
    --build) build="$2"; shift 2 ;;
    --commit) rev="$2"; shift 2 ;;
    --only-vesting) only_vesting=1; shift ;;
    --resume) resume=1; shift ;;
    --ids) out="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$build" in local|release) ;; *) echo "--build local|release" >&2; exit 2 ;; esac
network="local-$which_seq"
out="${out:-deploy/ids-$network.toml}"
case "$out" in /*) ;; *) out="$CV_REPO/$out" ;; esac

export LEE_WALLET_HOME_DIR="$CV_RUN_DIR/wallet-$network"
[ "$which_seq" = pub ] && unset RISC0_DEV_MODE
"$here/seq.sh" "$which_seq" status >/dev/null || { echo "$network is not running: scripts/seq.sh $which_seq start" >&2; exit 1; }
command -v wallet >/dev/null || { echo "wallet binary not on PATH (scripts/bootstrap.sh)" >&2; exit 1; }
export LEE_WALLET_PASSWORD="${LEE_WALLET_PASSWORD:-}"

mkdir -p "$(dirname "$out")"
exec 9>"$out.lock"
flock -w 900 9 || { echo "could not lock $out.lock within 15 min" >&2; exit 1; }

payer=cv-payer
if ! wallet account get --account-id "$payer" >/dev/null 2>&1; then
  key="$(grep -oE 'account import public --private-key [0-9a-f]{64}' "$CV_LEZ_DIR/Justfile" | head -1 | awk '{print $NF}')"
  [ -n "$key" ] || { echo "no local test key found in $CV_LEZ_DIR/Justfile" >&2; exit 1; }
  # rc3: `import public` has no --label; import, then label the printed id.
  id="$(wallet account import public --private-key "$key" | sed -n 's#.*Public/\([1-9A-HJ-NP-Za-km-z]\{32,44\}\).*#\1#p' | head -1)"
  [ -n "$id" ] || { echo "could not import the local test account" >&2; exit 1; }
  wallet account label --account-id "Public/$id" --label "$payer" >/dev/null
fi
payer_id="$(wallet account list | sed -n "s#^Public/\([1-9A-HJ-NP-Za-km-z]\{32,44\}\) \[$payer\].*#\1#p" | head -1)"
[ -n "$payer_id" ] || { echo "no account labelled $payer in the wallet" >&2; exit 1; }

# Host tools: the image id of a .bin, and the vesting_fees admin client (Init).
(cd "$CV_REPO" && RISC0_SKIP_BUILD=1 nice -n 10 cargo build -q --locked -p image_id -p vesting_fees_admin)
image_id() { "$CARGO_TARGET_DIR/debug/image-id" "$1" | sed -n 's/.*image id \([0-9a-f]\{64\}\)$/\1/p'; }

# ------------------------------------------------------------------ ids file
# Values kept between steps; written out after every deployment so a failure leaves a consistent file.
declare -A prog=() note=() img=()
accounts_fees_config=""; build_info=""
section_value() { # section key -> value of `key = "value"` in [section] of the current ids file
  [ -f "$out" ] || return 0
  awk -v s="[$1]" -v k="$2" '$0==s{f=1; next} /^\[/{f=0} f && $1==k {gsub(/"/, "", $3); print $3}' "$out"
}
section_comment() { # section key -> trailing comment of that line
  [ -f "$out" ] || return 0
  awk -v s="[$1]" -v k="$2" '$0==s{f=1; next} /^\[/{f=0} f && $1==k {sub(/^[^#]*# ?/, ""); print}' "$out"
}
for p in token vesting_fees ata vesting; do
  v="$(section_value programs "$p")"; [ -z "$v" ] && continue
  prog[$p]="$v"; note[$p]="$(section_comment programs "$p")"; img[$p]="$(section_value image_ids "$p")"
done
accounts_fees_config="$(section_value accounts fees_config)"

write_ids() {
  local tmp; tmp="$(mktemp "$out.XXXXXX")"
  {
    echo "# Program ids on $network, written by scripts/deploy_local.sh. Last update $(date -u +%FT%TZ)."
    echo "[deploy]"
    echo "network = \"$network\""
    echo "rpc = \"$(jq -r .sequencer.sequencer_addr "$LEE_WALLET_HOME_DIR/wallet_config.json")\""
    echo "lez = \"v0.4.0-rc3\""
    echo "dev_mode = $([ "$which_seq" = dev ] && echo true || echo false)"
    echo
    echo "# Program-loader header accounts (the program ids). vesting is built with the token, vesting_fees and ata"
    echo "# ids of this table; ata absent means it is built with its local test ATA id (token fees stay at rate 0)."
    echo "[programs]"
    for p in token vesting_fees ata vesting; do
      [ -n "${prog[$p]:-}" ] && printf '%s = "%s"   # %s\n' "$p" "${prog[$p]}" "${note[$p]}"
    done
    echo
    echo "[accounts]"
    [ -n "$accounts_fees_config" ] && echo "fees_config = \"$accounts_fees_config\"   # vesting_fees config (PDA), Init by the deployer"
    echo
    echo "# RISC Zero image ids of the deployed binaries (scripts/check_image_ids.sh <release dir> <this file>)."
    echo "[image_ids]"
    for p in token vesting_fees ata vesting; do
      [ -n "${img[$p]:-}" ] && printf '%s = "%s"\n' "$p" "${img[$p]}"
    done
    if [ -n "$build_info" ]; then echo; echo "[build]"; printf '%s\n' "$build_info"; fi
  } > "$tmp"
  chmod 644 "$tmp"
  mv -f "$tmp" "$out"
}

# ------------------------------------------------------------------ build
# build_guests [--ids FILE] -> prints the directory holding <program>.bin
build_guests() {
  if [ "$build" = release ]; then
    local log; log="$(mktemp)"
    "$here/guests_release.sh" "$@" "$rev" > "$log" 2>&1 || { cat "$log" >&2; exit 1; }
    cat "$log" >&2
    sed -n 's/^release set: //p; s/ already complete$//p' "$log" | tail -1
    rm -f "$log"
  else
    local inputs=()
    if [ "${1:-}" = --ids ]; then
      inputs=(CV_TOKEN_PROGRAM_ID="${prog[token]:-}" CV_FEES_PROGRAM_ID="${prog[vesting_fees]:-}")
      [ -n "${prog[ata]:-}" ] && inputs+=(CV_ATA_PROGRAM_ID="${prog[ata]}")
    fi
    (cd "$CV_REPO" && env -u CV_TOKEN_PROGRAM_ID -u CV_FEES_PROGRAM_ID -u CV_ATA_PROGRAM_ID "${inputs[@]}" \
      nice -n 10 cargo build --locked -q -p vesting_methods >&2)
    "$here/collect_guests.sh" vesting vesting_fees token >&2
    echo "$CARGO_TARGET_DIR/guests"
  fi
}

# ------------------------------------------------------------------ deploy
u32le() { od -An -tu4 -j "$2" -N4 "$1" | tr -d ' '; }
# ProgramBinary (risc0-binfmt): "R0BF" | u32 version | u32 header_len | header | u32 user_len | user ELF | kernel.
# The program loader splits the USER ELF into 96 KiB segments, at most 20.
user_elf_len() { local h; h=$(u32le "$1" 8); u32le "$1" $((12 + h)); }
new_public() { # label -> account id
  wallet account new public --label "$1" | sed -n 's#.*Public/\([1-9A-HJ-NP-Za-km-z]\{32,44\}\).*#\1#p' | head -1
}
stamp="$(date -u +%Y%m%dT%H%M%SZ)"

deploy() { # program bin_dir
  local p="$1" bin="$2/$1.bin" size user n header segs=()
  [ -f "$bin" ] || { echo "missing $bin" >&2; exit 1; }
  size=$(stat -c %s "$bin"); user=$(user_elf_len "$bin"); n=$(( (user + 98303) / 98304 ))
  [ "$n" -le 20 ] || { echo "$p user ELF is $user bytes: $n segments, above the 20-segment cap" >&2; exit 1; }
  header="$(new_public "cv-$p-header-$stamp")"
  for i in $(seq 1 "$n"); do segs+=("Public/$(new_public "cv-$p-seg$i-$stamp")"); done
  echo "deploying $p ($size bytes, user ELF $user bytes, $n segments) header $header"
  # The wallet prints every transaction in full; keep its hash and inclusion lines.
  wallet program-loader deploy --elf "$bin" --header "Public/$header" --segments "${segs[@]}" --immutable --payer "$payer" \
    | grep -v '^Transaction data is'
  prog[$p]="$header"
  img[$p]="$(image_id "$bin")"
  note[$p]="sha256 $(sha256sum "$bin" | cut -c1-64), $size bytes, $build build, $stamp"
  write_ids
}

kept() { [ "$resume" = 1 ] && [ -n "${prog[$1]:-}" ]; }
if [ "$only_vesting" = 0 ]; then
  base="$(build_guests)"
  kept token || deploy token "$base"
  if ! kept vesting_fees || [ -z "$accounts_fees_config" ]; then
    kept vesting_fees || deploy vesting_fees "$base"
    echo "vesting_fees Init by the deployer"
    "$CARGO_TARGET_DIR/debug/vesting-fees-admin" --program "${prog[vesting_fees]}" --payer "$payer_id" init
    accounts_fees_config="$("$CARGO_TARGET_DIR/debug/vesting-fees-admin" --program "${prog[vesting_fees]}" show \
      | sed -n 's/^config \([1-9A-HJ-NP-Za-km-z]\{32,44\}\): rate_bps.*/\1/p')"
    [ -n "$accounts_fees_config" ] || { echo "vesting_fees config is still empty after Init" >&2; exit 1; }
    write_ids
  fi
  if kept ata; then :
  elif [ -f "$base/ata.bin" ]; then deploy ata "$base"
  else unset 'prog[ata]' 'img[ata]'; write_ids; echo "no ata.bin in $base: ata not deployed (token fees stay at rate 0)"; fi
else
  [ -n "${prog[token]:-}" ] && [ -n "${prog[vesting_fees]:-}" ] \
    || { echo "--only-vesting needs token and vesting_fees in $out" >&2; exit 1; }
fi

vbins="$(build_guests --ids "$out")"
built_with="CV_TOKEN_PROGRAM_ID=${prog[token]} CV_FEES_PROGRAM_ID=${prog[vesting_fees]}"
[ -n "${prog[ata]:-}" ] && built_with="$built_with CV_ATA_PROGRAM_ID=${prog[ata]}"
deploy vesting "$vbins"
if [ "$build" = release ]; then
  build_info="builder = \"$(sed -n 's/^image=//p' "$vbins/BUILD_INFO")\"
commit = \"$(sed -n 's/^commit=//p' "$vbins/BUILD_INFO")\"
tree = \"$(sed -n 's/^tree=//p' "$vbins/BUILD_INFO")\"
vesting_inputs = \"$built_with\""
else
  build_info="builder = \"local rzup toolchain (image ids depend on the machine; --build release for reproducible ones)\"
vesting_inputs = \"$built_with\""
fi
write_ids
echo "wrote $out"
