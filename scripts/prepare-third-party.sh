#!/usr/bin/env bash
# Prepare the patched third-party source trees: for every submodule under
# third_party/<name> that has a queue in patches/<name>/, restore the files the
# queue touches to the pinned commit and apply the queue in filename order.
#
# Deterministic and idempotent: each run starts from pristine files, so a
# partially applied or hand-edited tree can't slip through. Never fix up files
# inside third_party/ by hand between runs; edit the patch instead.
#
# Usage: scripts/prepare-third-party.sh [name...]   (default: every queue with a submodule)
#
# The queues in patches/ that have no submodule (wayland, libepoxy, gtk4) are
# applied by their Nix packages, not by this script. See patches/README.md.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Paths a patch touches ('+++ b/x') and creates ('--- /dev/null' then '+++ b/x').
patched_files() { sed -n 's|^+++ b/||p' "$@" | sort -u; }
created_files() { awk '/^--- \/dev\/null/ { getline; sub(/^\+\+\+ b\//, ""); print }' "$@" | sort -u; }

if [ $# -gt 0 ]; then
  names=("$@")
else
  names=()
  for d in patches/*/; do
    n="$(basename "$d")"
    [ -e "third_party/$n/.git" ] && names+=("$n")
  done
fi

for name in "${names[@]}"; do
  tree="third_party/$name"
  queue=("$ROOT"/patches/"$name"/*.patch)
  [ -e "$tree/.git" ] || { echo "$tree missing: git submodule update --init $tree"; exit 1; }
  [ -e "${queue[0]}" ] || { echo "patches/$name: no patches"; continue; }

  # The tree must sit at the commit the superproject pins; a queue written
  # for one upstream commit says nothing about another.
  pinned="$(git ls-tree HEAD "$tree" | awk '{print $3}')"
  if [ -n "$pinned" ]; then
    actual="$(git -C "$tree" rev-parse HEAD)"
    if [ "$actual" != "$pinned" ]; then
      echo "$tree is at ${actual:0:12}, the superproject pins ${pinned:0:12}."
      echo "  git submodule update $tree   (or commit the new pin and refresh the queue)"
      exit 1
    fi
  fi

  echo "==> $name ($(git -C "$tree" rev-parse --short HEAD))"

  # Restore every tracked file any patch touches; delete files patches create.
  restore=()
  while IFS= read -r f; do restore+=("$f"); done < <(patched_files "${queue[@]}")
  if [ ${#restore[@]} -gt 0 ]; then
    tracked=()
    while IFS= read -r f; do tracked+=("$f"); done < <(git -C "$tree" ls-files -- "${restore[@]}")
    [ ${#tracked[@]} -gt 0 ] && git -C "$tree" checkout -q -- "${tracked[@]}"
  fi
  while IFS= read -r f; do rm -f "$tree/$f"; done < <(created_files "${queue[@]}")

  for p in "${queue[@]}"; do
    if err="$(git -C "$tree" apply --check "$p" 2>&1)"; then
      git -C "$tree" apply "$p"
      echo "    $(basename "$p"): applied"
    else
      echo "    $(basename "$p"): DOES NOT APPLY"
      printf '%s\n' "$err" | sed 's/^/      /'
      exit 1
    fi
  done
done
