#!/usr/bin/env bash
# Deletes the spike's repository and fork. Needs the delete_repo scope:
#   gh auth refresh -s delete_repo
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$here/lib.sh"

for full in "$FORK_OWNER/$REPO" "$ORG/$REPO"; do
  if repo_exists "$full"; then
    gh repo delete "$full" --yes
    log "deleted $full"
  else
    log "$full already gone"
  fi
done
