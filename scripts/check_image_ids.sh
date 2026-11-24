#!/usr/bin/env bash
# Compares the image ids of a scripts/guests_release.sh output dir with the expected ones (CI check that the guest
# build is reproducible).
#
#   scripts/check_image_ids.sh <release_dir> [<expected_file>]
#
# <expected_file> is program/IMAGE_IDS.expected (default; lines `name id`, the local-test-id build) or a
# deploy/ids-<network>.toml (its [image_ids] table, `name = "id"`, the build with that network's ids). Every program
# listed in the expected file must be present with the same id.
set -euo pipefail
dir="${1:?release dir}"; exp="${2:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/program/IMAGE_IDS.expected}"
got="$(sed -n 's/^\([a-z0-9_]*\)\.bin: .* image id \([0-9a-f]\{64\}\)$/\1 \2/p' "$dir/IMAGE_IDS")"
fail=0; n=0
while read -r name id; do
  case "$name" in ''|\#*) continue ;; esac
  n=$((n + 1))
  have="$(awk -v n="$name" '$1==n{print $2}' <<<"$got")"
  if [ "$have" = "$id" ]; then echo "OK       $name $id"
  else echo "MISMATCH $name expected $id got ${have:-<missing>}"; fail=1; fi
done < <(
  case "$exp" in
    *.toml) awk '$0=="[image_ids]"{f=1; next} /^\[/{f=0} f && NF>=3 && $2=="=" {gsub(/"/, "", $3); print $1, $3}' "$exp" ;;
    *) cat "$exp" ;;
  esac
)
[ "$n" -gt 0 ] || { echo "no image ids in $exp"; fail=1; }
exit $fail
