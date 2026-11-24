#!/usr/bin/env bash
# Deploys a second `vesting_fees` + `vesting` pair on local-dev for the fee-enabled end-to-end tests (`feeon_` in
# e2e/tests/fees_on.rs), switches its fee on, and hands its admin role towards the tests.
#
#   e2e/deploy_fees.sh [--ids FILE]      (default deploy/ids-local-dev-fees.toml)
#
#   1. The new ids file starts with the token and ATA programs of deploy/ids-local-dev.toml (shared), then
#      `scripts/deploy_local.sh dev --resume --ids FILE` deploys `vesting_fees` (and its `Init`) and a `vesting`
#      built against it.
#   2. A fresh treasury account is opened and funded (a fee transfer needs an existing account), and the admin (the
#      deployer) sets the rate to 50 basis points with that treasury.
#   3. The admin nominates the tests' funding account (LEZ's second published local test key, as e2e/src/lib.rs
#      reads it) as the next admin. The tests accept, hand the role to an account of their own, raise the rate above
#      the cap (refused, 28) and finally renounce. A renounced pair cannot be used for that test again: run this
#      script again for a fresh pair.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/../scripts/env.sh"
cd "$CV_REPO"

out=deploy/ids-local-dev-fees.toml
while [ $# -gt 0 ]; do
  case "$1" in
    --ids) out="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
base=deploy/ids-local-dev.toml
rate_bps=50

value() { # file section key -> value
  awk -v s="[$2]" -v k="$3" '$0==s{f=1; next} /^\[/{f=0} f && $1==k {gsub(/"/, "", $3); print $3}' "$1"
}
line() { # file section key -> the whole line
  awk -v s="[$2]" -v k="$3" '$0==s{f=1; next} /^\[/{f=0} f && $1==k {print}' "$1"
}

# scripts/deploy_local.sh builds the guests inside a command substitution, where a failed build does not stop it:
# refuse up front when Cargo.lock is not current, the one way that build fails after the first one passed.
cargo metadata --locked --offline --format-version 1 >/dev/null \
  || { echo "Cargo.lock is not current: update it before deploying" >&2; exit 1; }

# 1. Shared token and ATA, then a fresh vesting_fees + vesting.
for p in token ata; do
  [ -n "$(value "$base" programs "$p")" ] || { echo "no $p in $base (scripts/deploy_local.sh dev)" >&2; exit 1; }
done
{
  echo "# Fee-enabled pair on local-dev; token and ata shared with $base."
  echo "[programs]"
  line "$base" programs token
  line "$base" programs ata
  echo
  echo "[image_ids]"
  line "$base" image_ids token
  line "$base" image_ids ata
} > "$out"
"$CV_REPO/scripts/deploy_local.sh" dev --resume --ids "$out"

# 2. Treasury and rate, sent by the admin (the program's header account, whose key the deployer's wallet holds).
export LEE_WALLET_HOME_DIR="$CV_RUN_DIR/wallet-local-dev" LEE_WALLET_PASSWORD="${LEE_WALLET_PASSWORD:-}"
fees="$(value "$out" programs vesting_fees)"
payer="$(wallet account list | sed -n 's#^Public/\([1-9A-HJ-NP-Za-km-z]\{32,44\}\) \[cv-payer\].*#\1#p' | head -1)"
[ -n "$fees" ] && [ -n "$payer" ] || { echo "no vesting_fees in $out or no cv-payer in the wallet" >&2; exit 1; }
admin_tool="$CARGO_TARGET_DIR/debug/vesting-fees-admin"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
treasury="$(wallet account new public --label "cv-fees-treasury-$stamp" \
  | sed -n 's#.*Public/\([1-9A-HJ-NP-Za-km-z]\{32,44\}\).*#\1#p' | head -1)"
[ -n "$treasury" ] || { echo "could not open a treasury account" >&2; exit 1; }
wallet auth-transfer send --from cv-payer --to "Public/$treasury" --amount 1000000 | grep -v '^Transaction data is'
"$admin_tool" --program "$fees" --payer "$payer" set-fee --admin "$fees" --rate-bps "$rate_bps" --treasury "$treasury"

# 3. Nominate the tests' funding account (the second `account import public` key of the LEZ Justfile).
key="$(grep -oE 'account import public --private-key [0-9a-f]{64}' "$CV_LEZ_DIR/Justfile" | sed -n 2p | awk '{print $NF}')"
[ -n "$key" ] || { echo "no second local test key in $CV_LEZ_DIR/Justfile" >&2; exit 1; }
nominee="$(wallet account list | sed -n 's#^Public/\([1-9A-HJ-NP-Za-km-z]\{32,44\}\) \[cv-e2e-funder\].*#\1#p' | head -1)"
if [ -z "$nominee" ]; then
  nominee="$(wallet account import public --private-key "$key" \
    | sed -n 's#.*Public/\([1-9A-HJ-NP-Za-km-z]\{32,44\}\).*#\1#p' | head -1)"
  [ -n "$nominee" ] || { echo "could not read the funding account's id" >&2; exit 1; }
  wallet account label --account-id "Public/$nominee" --label cv-e2e-funder >/dev/null
fi
"$admin_tool" --program "$fees" --payer "$payer" nominate --admin "$fees" --next "$nominee"
echo "fee pair ready: $out (rate $rate_bps bps, treasury $treasury, nominee $nominee)"
