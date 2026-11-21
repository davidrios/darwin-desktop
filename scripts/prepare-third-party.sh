#!/usr/bin/env bash
# Build the patched third-party trees: for every submodule third_party/<name>
# with a queue in patches/<name>/, export the pinned commit into
# build/third_party/<name> and apply the queue in filename order.
#
# The submodules themselves stay pristine (the Nix build reads them as they are
# and applies the same queue). Deterministic: the copy comes from `git archive`
# of the pinned commit, so stray edits in the submodule can't leak in. A stamp
# (commit + patch hashes) skips the work when nothing changed, so Cargo doesn't
# rebuild the crate for nothing. Never edit build/third_party by hand: change
# the patch and rerun.
#
# Usage: scripts/prepare-third-party.sh [-f] [name...]   (default: every queue with a submodule)
#   -f  rebuild even if the stamp matches
#
# Queues without a submodule (wayland, libepoxy, gtk4) are applied by their Nix
# packages. See patches/README.md.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

force=0
if [ "${1:-}" = "-f" ]; then force=1; shift; fi

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
  out="build/third_party/$name"
  queue=("$ROOT"/patches/"$name"/*.patch)
  [ -e "$tree/.git" ] || { echo "$tree missing: git submodule update --init $tree"; exit 1; }
  [ -e "${queue[0]}" ] || queue=()

  # The submodule must sit at the commit the superproject pins: a queue
  # written for one upstream commit says nothing about another.
  pinned="$(git ls-tree HEAD "$tree" | awk '{print $3}')"
  actual="$(git -C "$tree" rev-parse HEAD)"
  if [ -n "$pinned" ] && [ "$actual" != "$pinned" ]; then
    echo "$tree is at ${actual:0:12}, the superproject pins ${pinned:0:12}."
    echo "  git submodule update $tree   (or commit the new pin and refresh the queue)"
    exit 1
  fi

  stamp="$actual $( [ ${#queue[@]} -gt 0 ] && shasum -a 256 "${queue[@]}" | awk '{print $1}' | tr '\n' ' ')"
  if [ "$force" = 0 ] && [ -f "$out/.prepared" ] && [ "$(cat "$out/.prepared")" = "$stamp" ]; then
    echo "==> $name (${actual:0:8}): up to date"
    continue
  fi

  echo "==> $name (${actual:0:8}) -> $out"
  rm -rf "$out"
  mkdir -p "$out"
  git -C "$tree" archive --format=tar "$actual" | tar -x -C "$out"

  for p in "${queue[@]}"; do
    if err="$(patch -p1 --dry-run --silent -d "$out" < "$p" 2>&1)"; then
      patch -p1 --silent --no-backup-if-mismatch -d "$out" < "$p"
      echo "    $(basename "$p"): applied"
    else
      echo "    $(basename "$p"): DOES NOT APPLY"
      printf '%s\n' "$err" | sed 's/^/      /'
      rm -rf "$out"
      exit 1
    fi
  done
  printf '%s\n' "$stamp" > "$out/.prepared"
done
