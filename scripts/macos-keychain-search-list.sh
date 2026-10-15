#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Put a keychain on the user's keychain search list for the length of a
# signing step, then put the original list back.
#
# codesign finds a signing identity only in keychains on the search list.
# Its --keychain option narrows the search to one of those keychains; it does
# not add a keychain that is off the list. The macOS release job imports the
# Developer ID identity into its own keychain, so that keychain must be on the
# list while Tauri runs codesign. The list is a per-account preference, and
# the runner may be a developer Mac, so the original list must come back.
#
#   macos-keychain-search-list.sh push <keychain> <state-file>
#     Save the current list to <state-file>, then put <keychain> first.
#   macos-keychain-search-list.sh restore <state-file>
#     Write the saved list back and delete <state-file>. Saved entries whose
#     file no longer exists are dropped. A missing <state-file> means push
#     never ran, and restore does nothing.

set -eu

usage() {
  echo "usage: $0 push <keychain> <state-file> | restore <state-file>" >&2
  exit 2
}

# One path per line, without the quotes and indentation `security` prints.
current_list() {
  security list-keychains -d user |
    sed -e 's/^[[:space:]]*"//' -e 's/"[[:space:]]*$//' |
    grep -v '^[[:space:]]*$' || true
}

case "${1:-}" in
  push)
    [ $# -eq 3 ] || usage
    keychain=$2
    state=$3
    if [ -e "$state" ]; then
      echo "$state exists: an earlier push was not restored" >&2
      exit 1
    fi
    current_list > "$state"
    set --
    while IFS= read -r kc; do
      [ "$kc" = "$keychain" ] || set -- "$@" "$kc"
    done < "$state"
    security list-keychains -d user -s "$keychain" "$@"
    echo "keychain search list: added $keychain for this job"
    ;;
  restore)
    [ $# -eq 2 ] || usage
    state=$2
    [ -f "$state" ] || exit 0
    set --
    while IFS= read -r kc; do
      if [ -f "$kc" ]; then
        set -- "$@" "$kc"
      fi
    done < "$state"
    security list-keychains -d user -s "$@"
    rm -f "$state"
    echo "keychain search list: restored"
    ;;
  *)
    usage
    ;;
esac
