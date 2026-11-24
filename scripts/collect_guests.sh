#!/usr/bin/env bash
# Copies the newest <name>.bin of each guest built by the `vesting_methods` crate (program/) into
# $CARGO_TARGET_DIR/guests and prints size, segment count (96 KiB each, cap 20 = 1,966,080 bytes) and sha256.
#
#   scripts/collect_guests.sh vesting [...]
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/env.sh"
dest="$CARGO_TARGET_DIR/guests"; mkdir -p "$dest"
for g in "$@"; do
  src="$(find "$CARGO_TARGET_DIR/riscv-guest" -type f -name "$g.bin" -path '*riscv32im-risc0-zkvm-elf*' -printf '%T@ %p\n' 2>/dev/null \
    | sort -nr | head -1 | cut -d' ' -f2-)"
  [ -n "$src" ] || { echo "no $g.bin under $CARGO_TARGET_DIR/riscv-guest (make guests)" >&2; exit 1; }
  cp "$src" "$dest/$g.bin"
  size=$(stat -c %s "$dest/$g.bin")
  printf '%-12s %9d bytes  %2d segments  %s\n' "$g" "$size" $(( (size + 98303) / 98304 )) "$(sha256sum "$dest/$g.bin" | cut -c1-16)"
done
