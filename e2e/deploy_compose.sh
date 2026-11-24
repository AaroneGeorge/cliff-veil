#!/usr/bin/env bash
# Deploys the launchpad_escrow example (examples/launchpad_escrow, docs/COMPOSITION.md) on local-dev for the
# composition end-to-end test (e2e/tests/compose.rs), against the `vesting` of deploy/ids-local-dev.toml.
#
#   e2e/deploy_compose.sh [--ids FILE]      (default deploy/ids-local-dev-compose.toml)
#
#   1. Builds the example guest with the local rzup toolchain and the build input CV_VESTING_PROGRAM_ID set to the
#      `vesting` of deploy/ids-local-dev.toml, so the launches it opens can only use that program.
#   2. Deploys it (immutable) and writes FILE: the example's id and image id, and the vesting, vesting_fees and
#      fees_config ids of deploy/ids-local-dev.toml it was built against.
#
# Run it after scripts/deploy_local.sh dev; run it again whenever `vesting` is redeployed.
set -euo pipefail
shopt -s inherit_errexit

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/../scripts/env.sh"
cd "$CV_REPO"

out=deploy/ids-local-dev-compose.toml
while [ $# -gt 0 ]; do
  case "$1" in
    --ids) out="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
base=deploy/ids-local-dev.toml

value() { # file section key -> value
  awk -v s="[$2]" -v k="$3" '$0==s{f=1; next} /^\[/{f=0} f && $1==k {gsub(/"/, "", $3); print $3}' "$1"
}
line() { # file section key -> the whole line
  awk -v s="[$2]" -v k="$3" '$0==s{f=1; next} /^\[/{f=0} f && $1==k {print}' "$1"
}

vesting="$(value "$base" programs vesting)"
[ -n "$vesting" ] || { echo "no vesting in $base (scripts/deploy_local.sh dev)" >&2; exit 1; }
cargo metadata --locked --offline --format-version 1 >/dev/null \
  || { echo "Cargo.lock is not current: update it before deploying" >&2; exit 1; }

# Hold the base file's lock (as scripts/deploy_local.sh does) so `vesting` is not redeployed meanwhile and the
# deployer's wallet is used by one deployment at a time.
exec 9>"$base.lock"
flock -w 900 9 || { echo "could not lock $base.lock within 15 min" >&2; exit 1; }

# 1. The guest, built against this network's vesting, and the image-id tool.
env -u CV_VESTING_PROGRAM_ID CV_VESTING_PROGRAM_ID="$vesting" \
  nice -n 10 cargo build --locked -q -p launchpad_escrow_methods
RISC0_SKIP_BUILD=1 nice -n 10 cargo build --locked -q -p image_id
"$CV_REPO/scripts/collect_guests.sh" launchpad_escrow
bin="$CARGO_TARGET_DIR/guests/launchpad_escrow.bin"
image="$("$CARGO_TARGET_DIR/debug/image-id" "$bin" | sed -n 's/.*image id \([0-9a-f]\{64\}\)$/\1/p')"
[ -n "$image" ] || { echo "no image id for $bin" >&2; exit 1; }

# 2. Deploy: a header account and one account per 96 KiB segment of the user ELF (as scripts/deploy_local.sh).
export LEE_WALLET_HOME_DIR="$CV_RUN_DIR/wallet-local-dev" LEE_WALLET_PASSWORD="${LEE_WALLET_PASSWORD:-}"
u32le() { od -An -tu4 -j "$2" -N4 "$1" | tr -d ' '; }
new_public() { # label -> account id
  wallet account new public --label "$1" | sed -n 's#.*Public/\([1-9A-HJ-NP-Za-km-z]\{32,44\}\).*#\1#p' | head -1
}
user=$(u32le "$bin" $((12 + $(u32le "$bin" 8))))
n=$(( (user + 98303) / 98304 ))
[ "$n" -le 20 ] || { echo "user ELF is $user bytes: $n segments, above the 20-segment cap" >&2; exit 1; }
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
header="$(new_public "cv-launchpad_escrow-header-$stamp")"
[ -n "$header" ] || { echo "could not open the header account" >&2; exit 1; }
segs=()
for i in $(seq 1 "$n"); do
  seg="$(new_public "cv-launchpad_escrow-seg$i-$stamp")"
  [ -n "$seg" ] || { echo "could not open segment account $i" >&2; exit 1; }
  segs+=("Public/$seg")
done
size=$(stat -c %s "$bin")
echo "deploying launchpad_escrow ($size bytes, user ELF $user bytes, $n segments) header $header"
wallet program-loader deploy --elf "$bin" --header "Public/$header" --segments "${segs[@]}" --immutable --payer cv-payer \
  | { grep -v '^Transaction data is' || true; }

tmp="$(mktemp "$out.XXXXXX")"
{
  echo "# The launchpad_escrow example on local-dev, written by e2e/deploy_compose.sh. Last update $(date -u +%FT%TZ)."
  echo "# vesting, vesting_fees and fees_config are those of $base it was built against."
  echo "[programs]"
  line "$base" programs vesting_fees
  line "$base" programs vesting
  printf 'launchpad_escrow = "%s"   # sha256 %s, %s bytes, local build, %s\n' \
    "$header" "$(sha256sum "$bin" | cut -c1-64)" "$size" "$stamp"
  echo
  echo "[accounts]"
  line "$base" accounts fees_config
  echo
  echo "[image_ids]"
  echo "launchpad_escrow = \"$image\""
  echo
  echo "[build]"
  echo "builder = \"local rzup toolchain (image ids depend on the machine)\""
  echo "launchpad_escrow_inputs = \"CV_VESTING_PROGRAM_ID=$vesting\""
} > "$tmp"
chmod 644 "$tmp"
mv -f "$tmp" "$out"
echo "wrote $out"
