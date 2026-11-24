#!/usr/bin/env bash
# Checks the commits of a range before they are pushed (pre-push hook, CI): one author and committer identity
# (scripts/hooks/identity.sh), no merge commits, no trailer lines, and the wording check on every message.
#
#   scripts/check_history.sh [RANGE]     default: every commit reachable from HEAD
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/hooks/identity.sh
source "$here/hooks/identity.sh"
range="${1:-HEAD}"
fail=0
n=0
while IFS=$'\t' read -r sha author committer parents; do
  n=$((n + 1))
  [ "$author" = "$CV_IDENTITY" ] || { echo "${sha:0:12}: author '$author'"; fail=1; }
  [ "$committer" = "$CV_IDENTITY" ] || { echo "${sha:0:12}: committer '$committer'"; fail=1; }
  [ "$(wc -w <<<"$parents")" -le 1 ] || { echo "${sha:0:12}: merge commit"; fail=1; }
  msg="$(git log -1 --format=%B "$sha")"
  t="$( { printf '%s\n' "$msg" | git interpret-trailers --parse; printf '%s\n' "$msg" | grep -i -E "$CV_TRAILER_RE"; } || true)"
  [ -z "$t" ] || { echo "${sha:0:12}: trailer lines:"; printf '%s\n' "$t" | sort -u | sed 's/^/  /'; fail=1; }
done < <(git log --format=$'%H\t%an <%ae>\t%cn <%ce>\t%P' "$range")
python3 -I "$here/hooks/wording.py" --log "$range" || fail=1
if [ "$fail" = 0 ]; then echo "check_history: $n commits in $range: one identity, no merges, no trailers, wording clean"
else echo "check_history: FAILED for $range" >&2; fi
exit "$fail"
