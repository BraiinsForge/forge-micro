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

# The gate's shell, which `nix/gate.nix` wraps. It is a file rather than
# a string in that module because the string cost it shellcheck in an
# editor, syntax highlighting and a readable diff, and the module gives
# up nothing by holding the wrapper alone.
#
# Eight values belong to nix and are set by the preamble that module
# prepends: `cores_per_job`, `event_prefix`, `build_activity`,
# `substitute_activity`, `builds_activity`, `progress_seconds`,
# `poll_seconds` and `nix_system`. They are assigned nowhere here, which
# is the point: each has one source.
# shellcheck shell=bash
# shellcheck disable=SC2154
# Resolved once. This form is correct from either half of a pair,
# which `--show-toplevel` alone is not: run inside a submodule it
# answers with the submodule, and the gate would then judge one
# tree while the caller meant the whole checkout.
root="$(git rev-parse --show-superproject-working-tree)"
root="${root:-$(git rev-parse --show-toplevel)}"

# Per worktree, and invisible to `git status`, so the gate's own
# files can never break the clean-tree assertion below.
state="$(git -C "$root" rev-parse --absolute-git-dir)/gate"

plan="$state/plan.tsv"
events="$state/events.jsonl"
summary="$state/summary.txt"
meta="$state/meta.tsv"

# The build events lifted out of the stream, and how far into the
# stream that lifting has read. See `fold_events` below.
folded="$state/builds.tsv"
cursor="$state/events.offset"

# The derivations whose build failed in this run, one to a line, written
# by `unit_ended` below. A check is a graph of units and most of them
# fail inside a unit, so this is what names a check's failure.
failed_units="$state/failed.txt"

# The tree the checks are evaluated against, written by
# `materialize` below. It hangs off the state directory, so it is
# per worktree and invisible to `git status` like everything else
# the gate writes.
src="$state/src"

jobs="${GATE_MAX_JOBS:-$(nproc)}"
cores="${GATE_CORES:-$cores_per_job}"

# How far back from the end of the event stream the run looks for the
# last count of builds. Nix sends one with every build that starts or
# ends, so the last megabyte holds one however many log lines arrived
# beside it.
progress_tail_bytes=1000000

# How many of the open units a quiet interval names, one to a line.
heartbeat_units=4

# How many of nix's own messages the summary carries when the build
# stopped and no check failed. Enough to hold a substituter error or a
# daemon refusal, which is what that state means.
message_lines=20

# What a forked evaluation exits with when nix refused a tree. `wait`
# hands the caller a status and nothing else, so the status is how it
# tells that from a fork that was killed. It is below the 126 and 127 a
# shell gives a command it could not run, and below the 128 a signal is
# added to, so it cannot be confused with either.
eval_failed=3

# What every evaluation of a tree needs: the per-unit builds of
# nix/units.nix plan with cargo during evaluation, which nix allows only
# when told so. A flake cannot say it for itself.
unsafe_eval=(--option allow-unsafe-native-code-during-evaluation true)

die() {
  printf '%s\n' "$@" >&2
  exit 1
}

# One line about what the run is doing now. It goes to stdout, beside
# the run's log and the table, because a reader watching the run
# is watching one stream and not two.
say() {
  printf '%s\n' "$*"
}

# Every submodule this repo declares, whether or not it carries a
# flake. All of them are materialized, because the outer flake may
# read a submodule's content whether or not that submodule is a
# flake itself, and all of them are preflighted, because a stale
# pin misrepresents the source either way.
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

mapfile -t DECLARED < <(declared_submodules)

# Of those, the ones whose checks this run builds. A submodule
# without a flake is content to the outer tree and nothing more,
# so it is carried into the source and never evaluated.
gated_submodules() {
  local path
  for path in "${DECLARED[@]}"; do
    if [ -e "$root/$path/flake.nix" ]; then
      printf '%s\n' "$path"
    fi
  done
}

# The gate reads the committed tree, so it refuses to judge one
# that is not committed. A refusal names the condition and the
# command that clears it, because the caller is usually an agent
# that cannot guess either.
preflight() {
  local path pin head

  for path in "${DECLARED[@]}"; do
    # An empty directory is a submodule that was never checked
    # out. That is a refusal rather than something to skip: the
    # alternative is a green run over a tree that is not there.
    if [ -z "$(ls -A "$root/$path" 2>/dev/null)" ]; then
      die "The gate did not start. The submodule $path is not checked out." \
          "The gate builds every tree of this checkout, so it must be present." \
          "Check it out at the pin:" \
          "  git -C $root submodule update --init"
    fi
  done

  # The submodules go before the outer tree. The outer status reports
  # a submodule that is dirty, or off its gitlink, as one modified
  # path, so tested first it caught every one of those under the
  # message below, and the two refusals that name the submodule and
  # the command that clears it were never reached.
  for path in "${DECLARED[@]}"; do
    if [ -n "$(git -C "$root/$path" status --porcelain)" ]; then
      die "The gate did not start. The submodule $path has uncommitted changes." \
          "The gate judges the committed tree, so it cannot see them." \
          "Commit the work first, then run the gate again." \
          "To check uncommitted work, use cargo check in the crate instead."
    fi

    pin="$(git -C "$root" rev-parse "HEAD:$path")"
    head="$(git -C "$root/$path" rev-parse HEAD)"
    if [ "$pin" != "$head" ]; then
      die "The gate did not start. A pin does not name its submodule head." \
          "The submodule is $path." \
          "The pin is $pin." \
          "The head is $head." \
          "The gate builds the checkout, so a stale pin gates content" \
          "that no merge will carry. Record the pin at the head:" \
          "  git -C $root add $path"
    fi
  done

  if [ -n "$(git -C "$root" status --porcelain)" ]; then
    die "The gate did not start. This tree has uncommitted changes." \
        "The gate judges the committed tree, so it cannot see them." \
        "Commit the work first, then run the gate again." \
        "To check uncommitted work, use cargo check in the crate instead."
  fi
}

# The source the checks read. `path:` copies a directory as it
# finds it, so aiming it at the checkout carries every ignored
# build artifact into the store and into any check that reads the
# whole tree. `git archive` writes the tracked content alone, and
# the preflight has already established that this is the working
# tree, so the copy loses nothing the gate should see.
#
# A nix archive records no timestamp, so two runs over identical
# content hash alike and the evaluation stays cached across the
# rewrite this directory gets on every run.
materialize() {
  local path
  rm -rf "$src"
  mkdir -p "$src"
  git -C "$root" archive HEAD | tar -x -C "$src"
  for path in "${DECLARED[@]}"; do
    mkdir -p "$src/$path"
    git -C "$root/$path" archive HEAD | tar -x -C "$src/$path"
  done
}

# Every tree whose checks this run builds, as a path into the
# materialized source, paired with the prefix its check names
# carry in the table. The outer tree's names are bare, so a
# standalone checkout reads exactly as it did before there was a
# submodule to distinguish them from.
gated_trees() {
  local path
  printf '%s\t%s\n' "$src" ""
  while read -r path; do
    printf '%s\t%s\n' "$src/$path" "$path/"
  done < <(gated_submodules)
}

# One tree's checks, as name, derivation and output. Forcing
# drvPath is what instantiates each derivation, which is what lets
# the build below name them by path and evaluate nothing at all.
#
# The string context has to go, or nix refuses to serialize a
# store path into JSON. The plan is a list of paths to look up
# rather than a dependency of anything, so discarding it loses
# nothing.
eval_tree() {
  local tree=$1 prefix=$2
  nix eval --json "path:$tree#checks.$nix_system" "${unsafe_eval[@]}" \
    --apply 'checks:
      builtins.mapAttrs (_: check: {
        drv = builtins.unsafeDiscardStringContext check.drvPath;
        out = builtins.unsafeDiscardStringContext check.outPath;
      }) checks' \
  | jq -r --arg prefix "$prefix" \
      'to_entries[] | [$prefix + .key, .value.drv, .value.out] | @tsv'
}

# The wiring that no check covers. `checks` is every gate, but a
# dev shell is an output like any other and a broken one is found
# by the developer who enters it rather than by the gate.
#
# It is the derivation of each shell that has to be forced, not the
# names. `builtins.attrNames` forces the attribute set and leaves
# every value a thunk, so a shell that does not evaluate passed
# this and was still found by the developer who entered it.
eval_shells() {
  local tree=$1
  nix eval "path:$tree#devShells.$nix_system" "${unsafe_eval[@]}" \
    --apply 'shells: builtins.mapAttrs (_: shell: shell.drvPath) shells' \
    > /dev/null
}

# One tree's whole evaluation, which is what `write_plan` forks. Both
# halves run here rather than in two passes over the trees: the shells
# used to be a second pass, serial across trees, after a first pass
# that was already parallel. That made a pair four evaluations with two
# of them in series, and each is about a minute cold.
#
# The two halves share one status. Nix's own message names the
# attribute it was evaluating, so the caller can already tell a check
# from a shell, and a second status would only say it again.
eval_one() {
  local tree=$1 prefix=$2 part=$3
  eval_tree "$tree" "$prefix" > "$part" || exit "$eval_failed"
  eval_shells "$tree" || exit "$eval_failed"
}

# One row per check, written before the build, so the table counts
# every check from the first tick of the build onward. It is not
# written from the moment the run starts: the evaluation comes first
# and there is no plan until it ends, which is what `status` reports
# instead. The trees evaluate at once because none reads another's
# flake, and evaluation is the run's whole fixed cost on a small
# change.
#
# The field separator is the tab `gated_trees` writes, because the
# default one is whitespace and would split a checkout whose path
# holds a space.
#
# Each evaluation writes its own stderr to the state directory, the
# same way the build writes its event stream there. What nix says
# while it evaluates is about the machine rather than about the
# branch: an untrusted substituter the flake asks for, a credential
# helper the environment supplies. None of that belongs in a table,
# and leaving it on the terminal put ten lines of it around five
# lines of result. It is kept rather than dropped, so a run that
# fails can say what nix said, and a reader who wants the rest knows
# where it is.
write_plan() {
  local tree prefix n=0 pids=() trees=() parts=() logs=() i part log rc
  while IFS=$'\t' read -r tree prefix; do
    part="$state/plan.$n"
    log="$state/eval.$n.log"
    eval_one "$tree" "$prefix" "$part" 2> "$log" &
    pids+=("$!")
    trees+=("$tree")
    parts+=("$part")
    logs+=("$log")
    n=$((n + 1))
  done < <(gated_trees)

  for i in "${!pids[@]}"; do
    # Which tree failed to evaluate and what nix said about it. Nix's
    # message alone names a store path the caller cannot place, so the
    # tree is named beside it. The last arm catches the status a signal
    # leaves, which is a run that was killed rather than a tree that
    # did not evaluate.
    rc=0
    wait "${pids[$i]}" || rc=$?
    case "$rc" in
      0) ;;
      "$eval_failed")
        die "The gate did not start. A tree did not evaluate." \
            "The tree is ${trees[$i]}." \
            "Nix said:" \
            "$(cat "${logs[$i]}")"
        ;;
      *)
        die "The gate did not start. A tree's evaluation did not finish." \
            "The tree is ${trees[$i]}." \
            "The evaluation exited with $rc. It was killed, or it stopped" \
            "before it could start." \
            "Nix said:" \
            "$(cat "${logs[$i]}")"
        ;;
    esac
  done
  cat "${parts[@]}" > "$plan"
  rm -f "${parts[@]}"
}

# The build activities of the run, lifted out of the event stream and
# kept as `id<TAB>action<TAB>drv`. nix prefixes every event line, so the
# prefix goes before jq reads it, and the grep drops the build log lines
# that make up most of the stream.
#
# Only start events are matched on their type. A result event carries a
# type of its own, and nix's progress results share their number with a
# build start, so a filter on the type alone kept every one of them and
# grew the fold by a line per progress update.
#
# A stop event carries an id and nothing else, no type among it, so it
# has to be matched on its action or every build looks like one that
# never finished. Stops of other activities come through with it and are
# harmless: only an id that a build start also claimed is ever read.
#
# The stream only grows, and it holds every build log line of every
# check, so a full run leaves tens of megabytes of it. Reading all of
# that on every tick made the cost of watching a run grow with the run.
# Each tick reads the bytes that arrived since the last one instead, and
# what it appends here is one line per build event rather than one per
# log line, so the file the rest of the gate reads stays small.
#
# Whole lines are what nix writes, one event to a write, and this reads
# them that way. The reader that came before it made the same
# assumption, because jq rejects half a line.
#
# The grep finds nothing in the first seconds of a run, when nix has
# written events of other kinds and no build has started. That is an
# empty result and not an error, so it is held away from the exit
# status: `pipefail` would otherwise carry it up through `summarize`
# and end the run with no message at all.
fold_events() {
  local offset=0 size
  # The run is the one writer of this pair, and `claim` is what makes it
  # so. A reader folds nothing: `gate status` from a second terminal
  # runs the same `summarize` the run's own tick runs, and two of them
  # appending to one file can interleave a write and carry the cursor
  # past bytes that never reached it. A reader sees the fold as the last
  # tick left it, which is what it saw between two ticks anyway, and a
  # run that has ended folded the whole stream before it wrote its
  # summary.
  if [ "$(cat "$state/pid" 2>/dev/null || true)" != "$$" ]; then
    return 0
  fi
  if [ ! -s "$events" ]; then
    return 0
  fi
  if [ -s "$cursor" ]; then
    offset="$(cat "$cursor")"
  fi
  size="$(wc -c < "$events")"
  if [ "$size" -le "$offset" ]; then
    return 0
  fi
  tail -c "+$((offset + 1))" "$events" \
  | { grep -E "\"action\":\"start\".*\"type\":($build_activity|$substitute_activity|$builds_activity)\}|\"action\":\"stop\"" || true; } \
  | sed "s/^$event_prefix//" \
  | jq -r '[.id, .action, (.fields[0] // ""), (.type // "")] | @tsv' \
  >> "$folded"
  printf '%s\n' "$size" > "$cursor"
}

# The paths of one activity kind that the stream reports as started,
# derivations for a build and store paths for a download. A derivation
# started by this run was built by it, and everything else that is valid
# in the store was already there, which is the difference between a
# cache hit and a rebuild, and the one number that says whether the gate
# is fast for the right reason.
started_of() {
  if [ ! -s "$folded" ]; then
    return 0
  fi
  awk -F'\t' -v kind="$1" \
    '$2 == "start" && $4 == kind && $3 != "" { print $3 }' "$folded" | sort -u
}

running_of() {
  if [ ! -s "$folded" ]; then
    return 0
  fi
  awk -F'\t' -v kind="$1" '
    $2 == "stop"                { stopped[$1] = 1 }
    $2 == "start" && $4 == kind { path[$1] = $3 }
    END {
      for (id in path) if (!(id in stopped) && path[id] != "") print path[id];
    }' "$folded" | sort -u
}

started_drvs() {
  started_of "$build_activity"
}

running_drvs() {
  running_of "$build_activity"
}

# What nix is doing beside the builds: the cache downloads the stream
# reports, with the first names of the open ones, and when no derivation
# builds and no download is open, the activity nix started last. A cold
# store spends its first minutes asking the caches and downloading the
# toolchains, and a line that counted builds alone read as a stalled run
# for all of that time.
activity() {
  local open running ended names last
  open="$(running_of "$substitute_activity")"
  running="$(printf '%s' "$open" | grep -c . || true)"
  ended="$(( $(started_of "$substitute_activity" | grep -c . || true) - running ))"
  if [ "$running" -gt 0 ] || [ -n "$(running_drvs)" ]; then
    names="$(printf '%s\n' "$open" | sed -E 's#^/nix/store/[a-z0-9]+-##' | head -n 3 | paste -sd ' ')"
    printf 'downloads done %d  open %d  %s' "$ended" "$running" "$names"
    return 0
  fi
  last="$(tail -n 50 "$events" 2>/dev/null \
    | { grep -F '"action":"start"' || true; } \
    | tail -n 1 \
    | sed "s/^$event_prefix//" \
    | jq -r '.text // empty' \
    | sed -E 's#/nix/store/[a-z0-9]+-##g')"
  printf 'now %s' "$last"
}

# Nix's own messages, which are the other thing the stream carries. A
# check that fails is read from the table and from its log. A run that
# stops without any check failing leaves nothing in either, and these
# are then the only account of what happened.
nix_messages() {
  if [ ! -s "$events" ]; then
    return 0
  fi
  { grep -F '"action":"msg"' "$events" || true; } \
  | sed "s/^$event_prefix//" \
  | jq -r '.msg // empty'
}

# A run has ended when it wrote its summary, and also when it is no
# longer going. The pid file is the liveness test: `claim` writes it and
# the EXIT trap removes it, so a file whose holder is gone is a run that
# was killed before it could write anything. Without the second test a
# killed run left every check it never reached reading `queued` for as
# long as the state directory stood, and `gate status` had no way to
# tell a dead run from a slow one. That reading is what `gate status`
# exists to give, so it has to be true of a run that died.
run_ended() {
  local holder
  if [ -s "$summary" ]; then
    return 0
  fi
  holder="$(cat "$state/pid" 2>/dev/null || true)"
  if [ -n "$holder" ] && kill -0 "$holder" 2>/dev/null; then
    return 1
  fi
  return 0
}

# One pass over the plan, the store and the event stream, emitting
# `state<TAB>name` for every check. The table and the run's log are
# both formatted from this, so the two can never disagree about
# what the run is doing.
#
# It asks the store which paths are missing rather than which are
# present: `nix path-info` fails the whole call on the first path
# it does not have, and a gate in progress always has some.
#
# A check that started, is no longer running and left no output has
# failed, and that is knowable while the rest of the gate is still
# building. Reading a failure from the end of the run instead would
# report it as queued for as long as the run lasts, which is the
# whole of what `--keep-going` is for. A check that never started
# and has no output is one whose dependency failed, so nix never
# built it. That is knowable as soon as a unit in its graph fails,
# from the derivations that refer to the failed unit, and of any other
# dependency only once the run has ended.
#
# The four lists reach awk as files, not as arguments: a check is a
# graph of hundreds of derivations, each started and stopped in the
# stream, and a list of them all is longer than one argument may be.
# Each call writes them to a directory of its own, because `gate status`
# from a second terminal runs this beside the run's own tick, and a
# list one of them empties while the other reads it shows every check
# as built.
summarize() {
  local lists invalid started running blocked
  fold_events
  lists="$(mktemp -d "$state/lists.XXXXXX")"
  invalid="$lists/invalid"
  started="$lists/started"
  running="$lists/running"
  blocked="$lists/blocked"
  cut -f3 "$plan" | xargs nix-store --check-validity --print-invalid > "$invalid"
  started_drvs > "$started"
  running_drvs > "$running"
  : > "$blocked"
  if [ -s "$failed_units" ]; then
    xargs nix-store -q --referrers-closure < "$failed_units" > "$blocked"
  fi

  awk -F'\t' \
    -v ended="$(run_ended && echo 1 || echo 0)" '
    FILENAME == ARGV[1] { if ($0 != "") isMissing[$0] = 1; next }
    FILENAME == ARGV[2] { if ($0 != "") wasStarted[$0] = 1; next }
    FILENAME == ARGV[3] { if ($0 != "") isRunning[$0] = 1; next }
    FILENAME == ARGV[4] { if ($0 != "") isBlocked[$0] = 1; next }
    {
      name = $1; drv = $2; out = $3;
      if (isRunning[drv])        state = "building";
      else if (!isMissing[out])  state = wasStarted[drv] ? "built" : "cached";
      else if (wasStarted[drv])  state = "FAILED";
      else if (isBlocked[drv])   state = "FAILED";
      else if (ended == 1)       state = "FAILED";
      else                       state = "queued";
      printf "%s\t%s\n", state, name;
    }' "$invalid" "$started" "$running" "$blocked" "$plan"
  rm -r "$lists"
}

# The table. Every check is in exactly one state, so the counts
# across the bottom always add up to the whole gate. Only what the
# reader can act on gets a row of its own: a hundred lines of
# cached and queued checks is what stops a status being read at a
# glance.
status() {
  if [ ! -s "$plan" ]; then
    # A run holds this worktree from its first second, and it writes no
    # plan until its evaluation ends. Between those two points there is
    # nothing to count, which is not the same as there being no run.
    # Reporting it as no run sent the reader to start one, which the
    # claim below then refused, and that pair of answers described a
    # healthy gate as a broken one.
    if ! run_ended; then
      say "gate: evaluating every tree, which is the slow part"
      say "No table is ready. The gate writes it when the evaluation ends."
      return 0
    fi
    die "No gate run was found for this worktree." \
        "Start one with: nix run .#gate"
  fi

  if [ -s "$meta" ]; then
    cat "$meta"
    printf '\n'
  fi

  summarize | awk -F'\t' '
    {
      count[$1]++;
      if ($1 == "building" || $1 == "FAILED") printf "%-9s %s\n", $1, $2;
    }
    END {
      printf "\ncached %d  built %d  building %d  queued %d  failed %d\n",
        count["cached"], count["built"], count["building"],
        count["queued"], count["FAILED"];
    }'

  if ! run_ended; then
    printf '%s\n' "$(activity)"
  fi

  if [ -s "$summary" ]; then
    printf '\n'
    cat "$summary"
  fi
}

# The run's own log, for the terminal the run itself is in: a line for
# every build and download that ends, a line for every check that
# passes or fails, and a line naming what still builds when nothing
# has ended for a while. It follows the folded stream from a line
# cursor of its own, so each event is printed once and as it lands. A
# line sampled on a timer showed whichever two units were building at
# that moment and lost the dozens that ended between two lines.
#
# The builds and downloads that are open live in a file, carried from
# one call to the next, because a build that starts in one poll ends in
# another. Most stop events in the stream belong to activities that are
# neither, and one awk pass matches them all against that file, so the
# shell sees only what ended.
declare -A check_state=()
open_units="$state/open.tsv"
streamed=0
last_line=0
run_start=0
builds_id=""
builds_done=0
builds_expected=0

elapsed() {
  printf '%d' "$(( $(date +%s) - run_start ))"
}

log_line() {
  local now
  now="$(elapsed)"
  printf '[%3d:%02d] %s\n' "$((now / 60))" "$((now % 60))" "$1"
  last_line=$now
}

short() {
  printf '%s' "$1" | sed -E 's#^/nix/store/[a-z0-9]+-##; s/\.drv$//'
}

# The platform a rostnix unit compiles for, which its derivation names
# and its name does not: one crate is a unit per target, and a log of
# names alone repeats it with nothing to tell the lines apart. A unit
# with no target compiles for this machine. Other derivations print
# nothing here.
target_of() {
  local found
  found="$(grep -o '\\"target\\":[^,]*' "$1" 2>/dev/null | head -n 1 || true)"
  case "$found" in
    '') ;;
    *null) printf 'host' ;;
    *) printf '%s' "$found" | sed -E 's/.*:\\"([^\\]*)\\".*/\1/' ;;
  esac
}

# The run's count of builds: the last progress nix gave its builds
# activity. Nix revises the total as it learns the graph, so the count is
# read again at every poll instead of being kept here.
read_builds() {
  local fields
  if [ -z "$builds_id" ]; then
    builds_id="$(awk -F'\t' -v kind="$builds_activity" \
      '$2 == "start" && $4 == kind { print $1; exit }' "$folded")"
  fi
  if [ -z "$builds_id" ]; then
    return 0
  fi
  fields="$(tail -c "$progress_tail_bytes" "$events" \
    | { grep -F "\"id\":$builds_id,\"type\":$build_activity}" || true; } \
    | tail -n 1 \
    | sed "s/^$event_prefix//" \
    | jq -r '.fields | "\(.[0]) \(.[1])"')"
  if [ -n "$fields" ]; then
    read -r builds_done builds_expected <<< "$fields"
  fi
}

# One unit as every line of the log names it: the name without the
# platform suffix a cross package set appends, which the target column
# says for every unit alike, and the target.
unit_label() {
  printf '%-44s %-26s' \
    "$(short "$1" | sed -E 's/-(arm|riscv64|x86_64|aarch64)-[a-z0-9_-]*$//')" \
    "$(target_of "$1")"
}

# How long a unit took, at the resolution the run sees: a start and a
# stop land in the run's view at a poll, so a unit that began and ended
# inside one poll reads as shorter than a poll rather than as no time.
duration() {
  if [ "$1" -lt "$poll_seconds" ]; then
    printf '<%ss' "$poll_seconds"
    return 0
  fi
  printf '%ss' "$1"
}

# A build that stopped either left its outputs in the store or failed.
# The stop event says neither, so the store answers, and a failure
# carries the first errors of its log, which is what a reader would
# otherwise run `nix log` for.
unit_ended() {
  local path=$1 kind=$2 took=$3 outputs=()
  if [ "$kind" = "$substitute_activity" ]; then
    log_line "fetched $(printf '%11s' '')  $(short "$path")"
    return 0
  fi
  mapfile -t outputs < <(nix-store -q --outputs "$path" 2>/dev/null || true)
  if [ "${#outputs[@]}" -gt 0 ] && nix-store --check-validity "${outputs[@]}" 2>/dev/null; then
    builds_done=$((builds_done + 1))
    log_line "$(printf 'built   %11s  %s %6s' \
      "$builds_done/$builds_expected" "$(unit_label "$path")" "$(duration "$took")")"
    return 0
  fi
  printf '%s\n' "$path" >> "$failed_units"
  log_line "$(printf 'FAIL    %11s  %s %6s' '' "$(unit_label "$path")" "$(duration "$took")")"
  { nix log "$path" 2>/dev/null || true; } \
  | { grep -m 3 -E '^error' || true; } \
  | sed 's/^/          /'
  printf '          log: nix log %s\n' "$path"
}

stream() {
  local total path kind took
  fold_events
  if [ ! -s "$folded" ]; then
    return 0
  fi
  total="$(wc -l < "$folded")"
  touch "$open_units"
  while IFS=$'\t' read -r path kind took; do
    unit_ended "$path" "$kind" "$took"
  done < <(
    tail -n "+$((streamed + 1))" "$folded" \
    | awk -F'\t' -v OFS='\t' -v now="$(elapsed)" -v next_open="$open_units.next" \
        -v build="$build_activity" -v fetch="$substitute_activity" '
      FILENAME == ARGV[1] { path[$1] = $2; kind[$1] = $3; since[$1] = $4; next }
      $2 == "start" && $3 != "" && ($4 == build || $4 == fetch) {
        path[$1] = $3; kind[$1] = $4; since[$1] = now; next
      }
      $2 == "stop" && ($1 in path) {
        print path[$1], kind[$1], now - since[$1];
        delete path[$1];
      }
      END {
        for (id in path) print id, path[id], kind[id], since[id] > next_open;
        close(next_open);
      }' "$open_units" -
  )
  mv "$open_units.next" "$open_units" 2>/dev/null || : > "$open_units"
  streamed=$total
  read_builds
}

# The checks' states as the run starts, and how many the store already
# held, so a warm run does not open with a line for every cached check.
seed_states() {
  local now name cached=0
  while IFS=$'\t' read -r now name; do
    check_state[$name]=$now
    if [ "$now" = cached ]; then
      cached=$((cached + 1))
    fi
  done < <(summarize)
  log_line "$cached of ${#check_state[@]} checks are already in the store"
}

# The checks that passed or failed since the last call.
check_states() {
  local now name
  while IFS=$'\t' read -r now name; do
    if [ "${check_state[$name]:-}" = "$now" ]; then
      continue
    fi
    check_state[$name]=$now
    case "$now" in
      built | cached) log_line "PASS    $name" ;;
      FAILED) log_line "FAIL    $name" ;;
    esac
  done < <(summarize)
}

# What still builds, said only when nothing else was said for a whole
# interval, so a long unit is not mistaken for a stalled run. Each open
# unit gets the same columns a finished one does, so two units of one
# crate read as two targets and not as one name printed twice.
heartbeat() {
  local count path
  if [ $(( $(elapsed) - last_line )) -lt "$progress_seconds" ]; then
    return 0
  fi
  count="$(grep -c . "$open_units" 2>/dev/null || true)"
  if [ "${count:-0}" -eq 0 ]; then
    log_line "...     $(activity)"
    return 0
  fi
  log_line "...     $count open, nothing ended in ${progress_seconds}s"
  while IFS= read -r path; do
    printf '         open    %11s  %s\n' '' "$(unit_label "$path")"
  done < <(cut -f2 "$open_units" | head -n "$heartbeat_units")
}

# `nix log` wants a derivation, and a caller has a check name. A check
# whose unit failed never ran and has no log of its own, so the log is
# then the failed units' in its graph, which hold the error.
show_log() {
  local name=$1 drv unit units=()
  drv="$(awk -F'\t' -v n="$name" '$1 == n { print $2 }' "$plan")"
  if [ -z "$drv" ]; then
    die "No check is named $name in this run." \
        "List the names with: nix run .#gate -- status"
  fi
  if nix log "$drv" 2>/dev/null; then
    return 0
  fi
  if [ -s "$failed_units" ]; then
    mapfile -t units < <(nix-store -qR "$drv" | grep -Fx -f "$failed_units" || true)
  fi
  if [ "${#units[@]}" -eq 0 ]; then
    die "The check $name has no build log." \
        "No build in its graph failed in this run."
  fi
  for unit in "${units[@]}"; do
    printf '==> %s\n' "$unit"
    nix log "$unit"
  done
}

# Two worktrees gate two branches and never meet here. Two callers
# in one worktree are the other case: they would gate the same
# commit and write to one state directory, so the second is not
# work to do in parallel, it is a second writer to one file. Send
# it to the run that is already going.
#
# The file is made rather than tested first. Two callers that start
# together both pass a test and both write, and then neither is the
# single writer the state directory needs. `set -C` makes the create
# fail for the second one instead, which is the claim itself.
claim() {
  local pidfile="$state/pid" holder
  if ! (set -C; printf '%s\n' "$$" > "$pidfile") 2>/dev/null; then
    holder="$(cat "$pidfile" 2>/dev/null || true)"
    if [ -n "$holder" ] && kill -0 "$holder" 2>/dev/null; then
      die "A gate is already running for this worktree, as pid $holder." \
          "It gates the same commit, so a second run would repeat it." \
          "Read the run that is going with:" \
          "  nix run .#gate -- status"
    fi
    # The holder is gone and left its file behind, which a killed run
    # does. Take the claim over, and fail if a third caller wins the
    # same race rather than writing beside it.
    rm -f "$pidfile"
    (set -C; printf '%s\n' "$$" > "$pidfile") 2>/dev/null \
      || die "A gate claimed this worktree while this run was starting." \
             "Read the run that is going with:" \
             "  nix run .#gate -- status"
  fi
  trap 'rm -f "$state/pid"' EXIT
}

run() {
  local path
  say "gate: preflight"
  preflight
  mkdir -p "$state"
  claim
  # Every file the last run left, and not only the two the build writes.
  # A plan and a meta that outlived the run that made them are what
  # `gate status` reads, so a run that died in evaluation used to leave
  # the previous commit's table standing, counted as fully cached, under
  # the previous commit's header. The clearing is after `claim`, because
  # until the claim holds this run is not the one writer of any of it.
  : > "$events"
  rm -f "$summary" "$plan" "$meta" "$folded" "$cursor" "$open_units" "$failed_units"

  # Every phase says that it started. The build is the observable part
  # of a run, but it is not the first part: a cold evaluation is about
  # a minute, and until it ends there is no plan to count and no table
  # to print. A run that says nothing for that minute cannot be told
  # from a run that has hung, which is the whole failure this gate was
  # built to remove.
  say "gate: reading the tracked content of the checkout"
  materialize
  say "gate: evaluating every tree, which is the slow part"
  write_plan

  # A plan with no rows would leave the build with no target, and
  # `nix build` with no target builds the default package of whatever
  # directory it is called from. That is a green gate over something
  # nobody asked for, so it stops here instead.
  if [ ! -s "$plan" ]; then
    die "The gate did not start. No tree reported a check to build." \
        "This means the flake evaluated and its \`checks\` are empty," \
        "which no tree of this checkout should be."
  fi

  {
    printf 'commit\t%s\n' "$(git -C "$root" rev-parse --short HEAD)"
    for path in "${DECLARED[@]}"; do
      printf 'pin %s\t%s\n' "$path" \
        "$(git -C "$root/$path" rev-parse --short HEAD)"
    done
    printf 'started\t%s\n' "$(date --iso-8601=seconds)"
    printf 'jobs\t%s x %s cores\n' "$jobs" "$cores"
    printf 'checks\t%s\n' "$(wc -l < "$plan")"
  } > "$meta"

  # By derivation path, so the build evaluates nothing. The plan
  # above is the run's only evaluation, and what it lists is
  # exactly what the build makes: a target named as a flake
  # attribute would evaluate the whole tree a second time, which
  # on a one-crate change is most of the run's wall clock.
  local targets=()
  mapfile -t targets < <(cut -f2 "$plan" | sed 's/$/^*/')

  say "gate: building ${#targets[@]} checks, $jobs jobs x $cores cores"

  local start rc build_pid
  start="$(date +%s)"
  rc=0
  # --keep-going because the run is observable: a failure lands in
  # the table while the rest of the gate continues, so one broken
  # check no longer hides the next one and costs a second run.
  nix build "${targets[@]}" "${unsafe_eval[@]}" \
    --no-link \
    --keep-going \
    --max-jobs "$jobs" \
    --cores "$cores" \
    --log-format internal-json \
    2> "$events" &
  build_pid=$!

  # The build owns stderr, which is the event stream, so this
  # terminal gets the run's log instead. A reader watching the run
  # sees it here, and a reader anywhere else runs `gate status`
  # against the same state.
  #
  # The stream is read every poll, so an event is printed within a
  # poll of landing and a finished run is noticed as soon. The checks'
  # states cost a store query over the whole plan, so they are read
  # once an interval. The last read is after the build ends, so the
  # final events are printed before the table.
  local waited=0
  run_start=$start
  seed_states
  while kill -0 "$build_pid" 2>/dev/null; do
    sleep "$poll_seconds"
    stream
    waited=$((waited + poll_seconds))
    if [ "$waited" -ge "$progress_seconds" ]; then
      waited=0
      check_states
      heartbeat
    fi
  done
  stream
  check_states
  wait "$build_pid" || rc=$?

  # Counted before the summary is written, because writing it is what
  # makes a run read as ended, and an ended run reports the checks that
  # never started as failed too. What is wanted here is the narrower
  # number: the checks that started and did not finish, and the checks
  # that a failed unit stopped.
  local failed
  failed="$(summarize | grep -c '^FAILED' || true)"

  {
    printf 'exit %s after %s s\n' "$rc" "$(( $(date +%s) - start ))"
    if [ "$rc" -eq 0 ]; then
      printf 'gate: PASS\n'
    else
      printf 'gate: FAIL\n'
      if [ "$failed" -gt 0 ]; then
        printf 'Read one failure with: nix run .#gate -- log <check>\n'
      else
        # Nix stopped and no check failed, so there is no log to name
        # and no row in the table to point at. A substituter that would
        # not answer, a full disk and a daemon that refused all end
        # here. Nix said why in the event stream, which nothing else
        # reads, so the summary carries the last of it.
        printf 'No check failed, so nix itself stopped the build.\n'
        printf 'Nix said:\n'
        nix_messages | tail -n "$message_lines" | sed 's/^/  /'
        printf 'The whole event stream is at:\n  %s\n' "$events"
      fi
    fi
  } > "$summary"

  status
  return "$rc"
}

case "${1:-run}" in
  run) run ;;
  status) status ;;
  log) show_log "${2:?a check name is required}" ;;
  *)
    die "Usage: gate [run|status|log <check>]" \
        "  run     preflight, then build every check of every tree" \
        "  status  print the table of this worktree's newest run" \
        "  log     print the build log of one check"
    ;;
esac
