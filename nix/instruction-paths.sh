# Copyright (C) 2026  Braiins Systems s.r.o.
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.
#
# Braiins Systems s.r.o. reserves the right to grant any party a license
# to this program, or any part thereof, under any terms, and such a grant
# shall be considered distinct from the grant above.

# Checks that every inline backticked path in CLAUDE.md and under
# .claude names something in the tree, because a dead path in an
# instruction file is the one prose defect a reader cannot see and a
# review cannot verify. A fenced block is command text rather than a
# token list, so its paths are outside the scan and are verified by
# running the recipe.
# Runs from the tree root. The first argument is the fewest instruction
# files the scan must reach; the rest are top-level names to skip.

min_files=$1
shift
skips=("$@")

skipped_top() {
  local s
  for s in "${skips[@]}"; do
    [ "$1" = "$s" ] && return 0
  done
  return 1
}

# find does not descend a symlinked root, so the scan counts the files
# it reaches rather than testing the roots. Its error on an absent root
# is silenced, so the messages below report that case instead of a raw
# find failure aborting the script.
scanned=$(find CLAUDE.md .claude -type f -name '*.md' 2>/dev/null | wc -l || true)
# CLAUDE.md is probed on its own, because a symlink there still leaves
# the aggregate count above the floor when .claude supplies enough
# files.
rootfile=$(find CLAUDE.md -maxdepth 0 -type f 2>/dev/null | wc -l || true)
if [ "$rootfile" -ne 1 ]; then
  echo "ERROR: CLAUDE.md is absent from the tree, or is a symlink the scan cannot follow." >&2
  exit 1
fi
if [ "$scanned" -lt "$min_files" ]; then
  echo "ERROR: the scan reached $scanned instruction files, fewer than $min_files." >&2
  echo ".claude is absent from the tree, or is a symlink the scan cannot follow." >&2
  exit 1
fi

dead=$(mktemp)
trap 'rm -f "$dead"' EXIT

while IFS= read -r file; do
  dir=$(dirname "$file")
  # A file with no backticked token makes grep exit 1, which is not a
  # failure. The backticks are the pattern, not a command substitution.
  # shellcheck disable=SC2016
  { grep -o '`[^` ]*`' "$file" || true; } | tr -d '`' | sort -u | while IFS= read -r p; do
    case "$p" in
      # Markup, a glob, a variable, a heading anchor, or a URL.
      *'<'* | *'*'* | *'$'* | *'#'* | *'('* | *'{'* | *://*) continue ;;
    esac
    # A file:line or file:line:col reference is a path reference.
    p=$(printf '%s' "$p" | sed 's/\(:[0-9]*\)*$//')
    top=${p%%/*}
    # A token needs a slash to be a path, and a leading slash makes a
    # slash command or an absolute path, neither of which is in the tree.
    [ "$top" != "$p" ] || continue
    [ -n "$top" ] || continue
    # A directory may be legitimately empty, like docs/plan/.
    case "$p" in */) continue ;; esac
    # A home-relative token names the operator's file, which no tree has.
    [ "$top" != "~" ] || continue
    if skipped_top "$top"; then
      continue
    fi
    # A token with one slash and no extension is prose: I/O, and/or,
    # origin/master. Only a token shaped like a path is held to the
    # existence tests below, so an empty tracked directory like
    # docs/plan passes with or without its trailing slash, since git
    # materializes no empty directory to probe.
    pathish=0
    case "$p" in */*/*) pathish=1 ;; esac
    case "${p##*/}" in *.*) pathish=1 ;; esac
    # A token that starts with ../ is relative to the file that names
    # it, and probing the root for it too would reach outside the tree.
    if [ "$top" = ".." ]; then
      [ -e "$dir/$p" ] || echo "$file: $p does not exist"
      continue
    fi
    # A first component that names no directory is reported rather than
    # skipped, so that a renamed directory fails every reference into it.
    if ! { [ -d "$top" ] || [ -d "$dir/$top" ]; }; then
      if [ "$pathish" -eq 1 ]; then
        echo "$file: $p is under $top/, which names no directory here"
      fi
      continue
    fi
    [ "$pathish" -eq 1 ] || continue
    [ -e "$p" ] || [ -e "$dir/$p" ] || echo "$file: $p does not exist"
  done
done < <(find CLAUDE.md .claude -type f -name '*.md' | sort) | tee "$dead"

if [ -s "$dead" ]; then
  echo "ERROR: an instruction file names a file that does not exist." >&2
  exit 1
fi
