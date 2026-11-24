#!/usr/bin/env bash
# Compiles examples/c/claim.c against the cliff-veil C ABI and runs it (in-memory chain), and checks that the header
# also compiles as C++ (Logos core modules are C++).
#
#   sdk/ffi/examples/c/run.sh <directory holding libcliff_veil.so>      (`make c-example` passes the cargo target)
set -euo pipefail

lib_dir="$(cd "${1:?usage: run.sh <directory holding libcliff_veil.so>}" && pwd)"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
include="$here/../../include"
out="$(mktemp -d "${TMPDIR:-/tmp}/cliff-veil-c.XXXXXX")"
trap 'rm -rf "$out"' EXIT

cc="${CC:-cc}"
cxx="${CXX:-c++}"
"$cc" -std=c11 -Wall -Wextra -Werror -pedantic -I "$include" "$here/claim.c" \
  -L "$lib_dir" -lcliff_veil -Wl,-rpath,"$lib_dir" -o "$out/claim"
printf '#include "cliff_veil.h"\nint main(void) { return cv_abi_version() == CV_ABI_VERSION ? 0 : 1; }\n' > "$out/check.cpp"
"$cxx" -std=c++17 -Wall -Wextra -Werror -I "$include" "$out/check.cpp" \
  -L "$lib_dir" -lcliff_veil -Wl,-rpath,"$lib_dir" -o "$out/check"
"$out/check"
echo "header compiles and links as C11 and C++17"
"$out/claim"
