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

# The commit linter's shell, which `nix/gitlint.nix` wraps. It lints
# the branch of every tree in the checkout against that tree's own
# review base, with that tree's own rule set, and reports one line per
# tree. One value belongs to nix and is set by the preamble that
# module prepends: `default_base`.
# shellcheck shell=bash
# shellcheck disable=SC2154
if [ "$#" -ne 0 ]; then
  printf '%s\n' "gitlint takes no argument. Each tree's range is its review base to HEAD." >&2
  exit 1
fi

# Resolved once, the way the gate does it: run inside a submodule,
# `--show-toplevel` answers with the submodule, and the run would then
# lint one tree while the caller meant the whole checkout.
root="$(git rev-parse --show-superproject-working-tree)"
root="${root:-$(git rev-parse --show-toplevel)}"

# Every submodule the root declares. A tree is linted when it carries
# a `.gitlint`, because a rule set is what makes a verdict this
# repository's own rather than the linter's defaults.
declared_submodules() {
  local modules="$root/.gitmodules" key path
  if [ ! -e "$modules" ]; then
    return 0
  fi
  while read -r key path; do
    if [ -n "$key" ] && [ -n "$path" ]; then
      printf '%s\n' "$path"
    fi
  done < <(
    git config --file "$modules" --get-regexp '^submodule\..*\.path$' || true
  )
}

# The review base of a tree: a stacked branch names its own in
# `branch.<name>.reviewBase`, and everything else is judged against
# the remote master. A detached head has no branch to look up and
# takes the default.
review_base() {
  local tree="$1" branch base
  branch="$(git -C "$tree" branch --show-current)"
  if [ -n "$branch" ]; then
    base="$(git -C "$tree" config "branch.$branch.reviewBase" || true)"
    if [ -n "$base" ]; then
      printf '%s\n' "$base"
      return 0
    fi
  fi
  printf '%s\n' "$default_base"
}

# One tree's verdict. The linter runs from inside the tree because its
# configuration names the rule directory by a relative path, and its
# output is kept whole: every line it prints is a finding.
lint_tree() {
  local label="$1" tree="$2" base count output status

  base="$(review_base "$tree")"
  if ! count="$(git -C "$tree" rev-list --count "$base..HEAD")"; then
    printf '%s: FAIL (the review base %s does not resolve)\n' "$label" "$base"
    return 1
  fi
  if [ "$count" -eq 0 ]; then
    printf '%s: no branch commits against %s\n' "$label" "$base"
    return 0
  fi

  status=0
  # The linter is told to ignore stdin, because it prefers stdin data
  # over `--commits` and this function inherits the submodule loop's
  # pipe as its stdin.
  output="$(cd "$tree" && gitlint --ignore-stdin --commits "$base..HEAD" 2>&1)" || status=$?
  if [ "$status" -eq 0 ]; then
    printf '%s: PASS (%s commits against %s)\n' "$label" "$count" "$base"
  else
    printf '%s: FAIL (%s commits against %s)\n' "$label" "$count" "$base"
    printf '%s\n' "$output"
  fi
  return "$status"
}

failed=0
if [ -e "$root/.gitlint" ]; then
  lint_tree "." "$root" || failed=1
fi
while read -r path; do
  if [ -e "$root/$path/.gitlint" ]; then
    lint_tree "$path" "$root/$path" || failed=1
  else
    # The skip is printed, because a silent one reads as a PASS over
    # commits that were never linted.
    printf '%s: SKIPPED (no .gitlint)\n' "$path"
  fi
done < <(declared_submodules)
exit "$failed"
