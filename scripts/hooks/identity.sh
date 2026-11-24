# shellcheck shell=bash
# shellcheck disable=SC2034  # used by the files that source this one
# The one author and committer identity of this repository. Sourced by the hooks and by scripts/check_history.sh.
CV_IDENTITY='Aarone George <96471433+AaroneGeorge@users.noreply.github.com>'
# Lines that are commit trailers ("Signed-off-by:", "Reviewed-by:", "Change-Id:", ...), case-insensitive.
CV_TRAILER_RE='^[[:space:]]*([A-Za-z][A-Za-z0-9-]*-by|change-id)[[:space:]]*:'
