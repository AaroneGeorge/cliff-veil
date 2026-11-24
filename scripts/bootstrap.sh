#!/usr/bin/env bash
# Sets up a machine (or a clean clone, or a CI runner) for cliff-veil: toolchains, the pinned LEZ checkout, the LEZ
# release binaries (standalone sequencer and wallet), build and run directories.
#
#   scripts/bootstrap.sh [--check] [--no-lez-build] [--with-sudo]
#
#   --check         report what is missing, change nothing (exit 1 if anything is missing)
#   --no-lez-build  do not build the LEZ binaries (about 20 min on 4 cores)
#   --with-sudo     run the apt-get step (it is always printed first); without it, only print it
#
# Layout under $B: see scripts/env.sh. An existing LEZ checkout at the pinned commit can be reused by pointing
# $B/vendor/lez at it (symlink) or by setting CV_LEZ_DIR; it is only read, never modified, unless the binaries are
# missing and have to be built.
#
# Versions: host Rust from rust-toolchain.toml (1.98.1, via rustup); rzup components cargo-risczero 3.0.5,
# r0vm 3.0.5, rust 1.91.1 (guest toolchain), cpp 2024.1.5. Docker is needed only for the reproducible guest build
# (scripts/guests_release.sh).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/env.sh"
check=0; lez_build=1; with_sudo=0
for a in "$@"; do
  case "$a" in
    --check) check=1 ;; --no-lez-build) lez_build=0 ;; --with-sudo) with_sudo=1 ;;
    *) echo "unknown argument: $a" >&2; exit 2 ;;
  esac
done
missing=0
say() { printf '[bootstrap] %s\n' "$*"; }
need() { say "MISSING: $*"; missing=1; }

say "repo $CV_REPO"; say "build dir B=$B"
[ "$check" = 1 ] || mkdir -p "$B/vendor" "$CV_RUN_DIR" "$CARGO_TARGET_DIR"

# ---- 1. system packages (sudo: printed, run only with --with-sudo) ----
apt_pkgs="build-essential clang libclang-dev libssl-dev pkg-config cmake jq libpcsclite-dev curl git"
apt_missing=""
for p in $apt_pkgs; do dpkg -s "$p" >/dev/null 2>&1 || apt_missing="$apt_missing $p"; done
if [ -n "$apt_missing" ]; then
  cmd="sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends$apt_missing"
  say "system packages needed: $cmd"
  if [ "$with_sudo" = 1 ] && [ "$check" = 0 ]; then sudo apt-get update -q && $cmd
  else need "apt packages:$apt_missing (rerun with --with-sudo, or run the line above)"; fi
fi
docker version >/dev/null 2>&1 || say "NOTE: docker not usable by $(id -un); needed only for scripts/guests_release.sh"

# ---- 2. pinned checkouts ----
while read -r dir repo_name ref commit; do
  case "$dir" in ''|\#*) continue ;; esac
  dest="$B/vendor/$dir"
  [ "$dir" = lez ] && dest="$CV_LEZ_DIR"
  if [ -d "$dest/.git" ] || [ -f "$dest/.git" ]; then
    have="$(git -C "$dest" rev-parse HEAD)"
    [ "$have" = "$commit" ] || { say "$dest is at $have, pinned $commit ($repo_name $ref): fix by hand"; exit 1; }
    say "ok $dest = $repo_name $ref"
    continue
  fi
  [ "$check" = 1 ] && { need "$dest ($repo_name $ref)"; continue; }
  say "fetch $dest = $repo_name $ref ($commit)"
  git init -q "$dest"
  git -C "$dest" remote add origin "https://github.com/$repo_name"
  git -C "$dest" fetch -q --depth 1 origin "$commit"
  git -C "$dest" checkout -q FETCH_HEAD
  [ "$(git -C "$dest" rev-parse HEAD)" = "$commit" ] || { say "commit mismatch for $dest"; exit 1; }
done < "$here/pins.txt"

# ---- 3. toolchains ----
command -v rustup >/dev/null || need "rustup (https://rustup.rs): the host toolchain comes from rust-toolchain.toml"
if command -v rustup >/dev/null && [ "$check" = 0 ]; then (cd "$CV_REPO" && rustup show active-toolchain >/dev/null 2>&1 || rustup toolchain install); fi
if ! command -v rzup >/dev/null; then
  if [ "$check" = 1 ]; then need "rzup"; else
    say "installing rzup (RISC Zero's installer; writes ~/.risc0)"
    curl -sSfL https://risczero.com/install -o "$TMPDIR/rzup-install.sh" && bash "$TMPDIR/rzup-install.sh"
    export PATH="$HOME/.risc0/bin:$PATH"
  fi
fi
rz_have() { rzup show 2>/dev/null | awk -v c="$1" '$0==c{f=1;next} /^[a-z]/{f=0} f' | grep -q -- "$2"; }
for cv in "cargo-risczero 3.0.5" "r0vm 3.0.5" "rust 1.91.1" "cpp 2024.1.5"; do
  set -- $cv
  if command -v rzup >/dev/null && rz_have "$1" "$2"; then continue; fi
  if [ "$check" = 1 ]; then need "rzup $1 $2"; else say "rzup install $1 $2"; rzup install "$1" "$2"; fi
done
if command -v rzup >/dev/null && [ "$check" = 0 ]; then
  rzup default cargo-risczero 3.0.5 >/dev/null 2>&1 || true
  rzup default r0vm 3.0.5 >/dev/null 2>&1 || true
fi

# ---- 4. LEZ rc3 binaries: standalone sequencer and wallet ----
for b in sequencer_service wallet; do
  [ -x "$CV_LEZ_BIN/$b" ] && continue
  if [ "$check" = 1 ] || [ "$lez_build" = 0 ]; then need "$CV_LEZ_BIN/$b"; continue; fi
  say "building LEZ binaries in $CV_LEZ_DIR (nice -n 15; about 20 min on 4 cores)"
  (cd "$CV_LEZ_DIR" && env -u CARGO_TARGET_DIR nice -n 15 cargo build --release --features standalone -p sequencer_service \
     && env -u CARGO_TARGET_DIR nice -n 15 cargo build --release -p wallet)
  if [ "$CV_LEZ_BIN" != "$CV_LEZ_DIR/target/release" ]; then
    mkdir -p "$CV_LEZ_BIN"
    cp "$CV_LEZ_DIR/target/release/sequencer_service" "$CV_LEZ_DIR/target/release/wallet" "$CV_LEZ_BIN/"
  fi
  break
done

if [ "$missing" = 1 ]; then say "some items are missing (see above)"; exit 1; fi
say "OK. Next: scripts/seq.sh dev start"
