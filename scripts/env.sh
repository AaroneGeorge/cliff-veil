# shellcheck shell=bash
# Source me: `source scripts/env.sh`. Sets the build environment every Makefile recipe and script uses.
#
# Layout (B = $CV_BUILD_DIR, else the repo's parent directory; scripts/bootstrap.sh creates it):
#   $B/vendor/lez         LEZ v0.4.0-rc3 checkout (scripts/pins.txt); may be a symlink to an existing checkout
#   $B/build/target       cargo target dir
#   $B/build/tmp          temp files (kept on disk, not on a tmpfs)
#   $B/run                sequencer homes, wallets and logs (scripts/seq.sh); optional env.local sourced last

_cv_repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CV_REPO="$_cv_repo"
export B="${CV_BUILD_DIR:-$(cd "$_cv_repo/.." && pwd)}"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$B/build/target}"
# Two jobs by default: builds share the machine with running sequencers. Raise it on a dedicated machine.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export TMPDIR="$B/build/tmp"
mkdir -p "$TMPDIR"

# Local runs use dev-mode receipts; a sequencer started with dev mode unset (scripts/seq.sh pub) clears it.
export RISC0_DEV_MODE=1
# Guest builder image, same tag as the LEZ v0.4.0-rc3 Justfile.
export RISC0_DOCKER_CONTAINER_TAG=r0.1.91.1

# LEZ checkout and the directory holding its release binaries (sequencer_service, wallet).
export CV_LEZ_DIR="${CV_LEZ_DIR:-$B/vendor/lez}"
export CV_LEZ_BIN="${CV_LEZ_BIN:-$CV_LEZ_DIR/target/release}"
export CV_RUN_DIR="${CV_RUN_DIR:-$B/run}"
# Machine-local overrides (for example CV_SEQ_UNIT_DEV for a sequencer started under another unit name), outside the repo.
# shellcheck disable=SC1091
[ -f "$CV_RUN_DIR/env.local" ] && source "$CV_RUN_DIR/env.local"

# Tools: rzup (cargo-risczero, the risc0 guest toolchain), cargo bins, LEZ release binaries.
for _p in "$HOME/.risc0/bin" "$HOME/.cargo/bin" "$CV_LEZ_BIN"; do
  case ":$PATH:" in *":$_p:"*) ;; *) [ -d "$_p" ] && PATH="$_p:$PATH" ;; esac
done
export PATH
unset _p _cv_repo
