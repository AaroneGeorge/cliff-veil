#!/usr/bin/env bash
# Reproducible release set of the program guests (vesting, vesting_fees, token, ata), built from the COMMITTED tree in
# the pinned image risczero/risc0-guest-builder:r0.1.91.1 (same tag as LEZ v0.4.0-rc3), into
# $B/build/guests-release/<commit>[-<network>-<inputs>]/ with SHA256SUMS, IMAGE_IDS and BUILD_INFO.
#
#   scripts/guests_release.sh [--ids FILE] [<commit>]       # default HEAD
#
# Two id sets (SPEC 1.1):
#   - no --ids: `vesting` is built with its local test ids (CV_* unset). These are the image ids of
#     program/IMAGE_IDS.expected, which CI checks with scripts/check_image_ids.sh.
#   - --ids deploy/ids-<network>.toml: `vesting` is built with the token, vesting_fees and ata ids recorded there,
#     passed into the container as CV_TOKEN_PROGRAM_ID, CV_FEES_PROGRAM_ID, CV_ATA_PROGRAM_ID (base58). A program
#     missing from the file leaves its variable unset. The image ids of that network are recorded in the same file
#     ([image_ids]) by scripts/deploy_local.sh and checked with `scripts/check_image_ids.sh <dir> <ids file>`.
#   vesting_fees, token and ata have no build inputs: their image ids are the same in both sets. ata is the vendored
#   ATA program (program/ata-guest, its own workspace and lock file).
#
# Same environment as risc0-build 3.0.5's own docker build (image, rustflags, RISC0_FEATURE_bigint2, the RISC Zero C
# compiler, workdir /src, CARGO_TARGET_DIR=target, `cargo +risc0 build --release --locked`), run with `docker run`
# rather than `docker build` so the container gets a hard memory limit: CV_GUEST_MEMORY_MB (default 2500) plus
# CV_GUEST_SWAP_MB (default 1024) of swap. Every guest is built in one container after one `cargo fetch`; the cargo
# registry and git caches are kept in two named docker volumes between runs (paths inside the container are
# unchanged, so the image ids do not depend on them).
#
# CV_RELEASE_DIR overrides the output dir. ELFs are wrapped with the v1compat kernel by tools/image_id, as
# risc0-build does, which also prints the image id.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/env.sh"

# Guest packages and their bins: "manifest bin [bin...]".
GUESTS=(
  "program/guest/Cargo.toml vesting"
  "program/fees/guest/Cargo.toml vesting_fees"
  "program/token-guest/Cargo.toml token"
  "program/ata-guest/guest/Cargo.toml ata"
)

ids_file=""; rev=HEAD
while [ $# -gt 0 ]; do
  case "$1" in
    --ids) ids_file="$2"; shift 2 ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) rev="$1"; shift ;;
  esac
done

# Build inputs of `vesting` from the [programs] table of an ids file.
inputs=()
if [ -n "$ids_file" ]; then
  [ -f "$ids_file" ] || { echo "no ids file $ids_file" >&2; exit 1; }
  program_id() { awk -v k="$1" '$0=="[programs]"{f=1; next} /^\[/{f=0} f && $1==k {gsub(/"/, "", $3); print $3}' "$ids_file"; }
  for pair in token:CV_TOKEN_PROGRAM_ID vesting_fees:CV_FEES_PROGRAM_ID ata:CV_ATA_PROGRAM_ID; do
    v="$(program_id "${pair%%:*}")"
    [ -z "$v" ] || inputs+=("${pair#*:}=$v")
  done
fi

commit="$(git -C "$CV_REPO" rev-parse "$rev^{commit}")"
tag="${RISC0_DOCKER_CONTAINER_TAG:-r0.1.91.1}"
# Pinned by digest as well as by tag; CV_GUEST_IMAGE overrides both.
image="${CV_GUEST_IMAGE:-risczero/risc0-guest-builder:$tag@sha256:fafb377a44e1cfca415577c48d2f7012bda99ed36f2fae27f9a663b9fe6048f0}"
suffix=""
if [ -n "$ids_file" ]; then
  network="$(basename "$ids_file" .toml)"; network="${network#ids-}"
  suffix="-$network-$(printf '%s\n' "${inputs[@]}" | sha256sum | cut -c1-8)"
fi
out="${CV_RELEASE_DIR:-$B/build/guests-release/$commit$suffix}"
src="$TMPDIR/guests-src-${commit:0:12}"
[ -e "$out/SHA256SUMS" ] && { echo "$out already complete"; cat "$out/IMAGE_IDS"; exit 0; }
mkdir -p "$out/elf"

# Committed tree only: no untracked or modified file reaches the build.
rm -rf "$src"; mkdir -p "$src"
git -C "$CV_REPO" archive --format=tar "$commit" | tar -x -C "$src"

T=riscv32im-risc0-zkvm-elf
us=$'\x1f'
rustflags="-C${us}passes=lower-atomic${us}-C${us}link-arg=-Ttext=0x00200800${us}-C${us}link-arg=--fatal-warnings${us}-C${us}panic=abort${us}--cfg${us}getrandom_backend=\"custom\""
script="set -eu
first=\$(echo '${GUESTS[0]}' | cut -d' ' -f1)
cargo +risc0 fetch --locked --target $T --manifest-path \"\$first\"
mkdir -p /src/.out
for g in $(printf "'%s' " "${GUESTS[@]}"); do
  set -- \$g; m=\$1; shift
  CARGO_MANIFEST_PATH=\$m cargo +risc0 build --release --locked --target $T --manifest-path \$m
  for b in \"\$@\"; do cp target/$T/release/\$b /src/.out/; done
done
chown -R $(id -u):$(id -g) /src"

docker image inspect "$image" >/dev/null 2>&1 || docker pull -q "$image"
digest="$image"
mem="${CV_GUEST_MEMORY_MB:-2500}"; swap="${CV_GUEST_SWAP_MB:-1024}"
env_inputs=()
for i in "${inputs[@]}"; do env_inputs+=(-e "$i"); done
echo "building guests of $commit${inputs[*]:+ with ${inputs[*]}} in $digest (memory ${mem} MiB + ${swap} MiB swap; log $out/docker.log)"
docker run --rm --name "cv-guests-${commit:0:12}" \
  --memory "${mem}m" --memory-swap "$((mem + swap))m" \
  --cpus "${CARGO_BUILD_JOBS:-2}" \
  -v "$src:/src" -w /src \
  -v cv-guest-cargo-registry:/root/.cargo/registry -v cv-guest-cargo-git:/root/.cargo/git \
  -e CARGO_ENCODED_RUSTFLAGS="$rustflags" -e CARGO_TARGET_DIR=target -e RISC0_FEATURE_bigint2= \
  -e CC_riscv32im_risc0_zkvm_elf=/root/.risc0/cpp/bin/riscv32-unknown-elf-gcc \
  -e CFLAGS_riscv32im_risc0_zkvm_elf="-march=rv32im -nostdlib" -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" \
  "${env_inputs[@]}" \
  --entrypoint /bin/sh "$image" -c "$script" > "$out/docker.log" 2>&1 || { tail -30 "$out/docker.log" >&2; exit 1; }
cp "$src"/.out/* "$out/elf/"

# Wrap with the v1compat kernel and record the image ids. tools/image_id is built from the committed tree as well.
(cd "$src" && CARGO_TARGET_DIR="$CARGO_TARGET_DIR" RISC0_SKIP_BUILD=1 nice -n 15 cargo build -q --locked -p image_id)
: > "$out/IMAGE_IDS"
for g in "${GUESTS[@]}"; do
  set -- $g; shift
  for b in "$@"; do "$CARGO_TARGET_DIR/debug/image-id" "$out/elf/$b" "$out/$b.bin" | sed "s#^$out/##" >> "$out/IMAGE_IDS"; done
done
(cd "$out" && sha256sum ./*.bin elf/* > SHA256SUMS)
{
  echo "commit=$commit"
  echo "image=$digest"
  echo "built_utc=$(date -u +%FT%TZ)"
  echo "tree=$(git -C "$CV_REPO" rev-parse "$commit^{tree}")"
  echo "machine=$(uname -srm)"
  echo "inputs=${inputs[*]:-none (local test ids)}"
} > "$out/BUILD_INFO"
rm -rf "$src"
cat "$out/IMAGE_IDS"
echo "release set: $out"
