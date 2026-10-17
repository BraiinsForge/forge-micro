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
# Six values belong to nix and are set by the preamble that module
# prepends: `cores_per_job`, `event_prefix`, `build_activity`,
# `progress_seconds`, `poll_seconds` and `nix_system`. They are assigned
# nowhere here, which is the point: each has one source.
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

# The tree the checks are evaluated against, written by
# `materialize` below. It hangs off the state directory, so it is
# per worktree and invisible to `git status` like everything else
# the gate writes.
src="$state/src"

jobs="${GATE_MAX_JOBS:-$(nproc)}"
cores="${GATE_CORES:-$cores_per_job}"

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
# the progress lines and the table, because a reader watching the run
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
  | { grep -E "\"action\":\"start\".*\"type\":$build_activity\}|\"action\":\"stop\"" || true; } \
  | sed "s/^$event_prefix//" \
  | jq -r '[.id, .action, (.fields[0] // "")] | @tsv' \
  >> "$folded"
  printf '%s\n' "$size" > "$cursor"
}

# A derivation that the event stream reports as started was built
# by this run. Everything else that is valid in the store was
# already there, which is the difference between a cache hit and a
# rebuild, and the one number that says whether the gate is fast
# for the right reason.
started_drvs() {
  if [ ! -s "$folded" ]; then
    return 0
  fi
  awk -F'\t' '$2 == "start" && $3 != "" { print $3 }' "$folded" | sort -u
}

running_drvs() {
  if [ ! -s "$folded" ]; then
    return 0
  fi
  awk -F'\t' '
    $2 == "stop"  { stopped[$1] = 1 }
    $2 == "start" { drv[$1] = $3 }
    END {
      for (id in drv) if (!(id in stopped) && drv[id] != "") print drv[id];
    }' "$folded" | sort -u
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
# `state<TAB>name` for every check. The table and the progress line
# are both formatted from this, so the two can never disagree about
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
# built it; that is only knowable once the run has ended.
#
# The three lists reach awk as files, not as arguments: a check is a
# graph of hundreds of derivations, each started and stopped in the
# stream, and a list of them all is longer than one argument may be.
# Each call writes them to a directory of its own, because `gate status`
# from a second terminal runs this beside the run's own tick, and a
# list one of them empties while the other reads it shows every check
# as built.
summarize() {
  local lists invalid started running
  fold_events
  lists="$(mktemp -d "$state/lists.XXXXXX")"
  invalid="$lists/invalid"
  started="$lists/started"
  running="$lists/running"
  cut -f3 "$plan" | xargs nix-store --check-validity --print-invalid > "$invalid"
  started_drvs > "$started"
  running_drvs > "$running"

  awk -F'\t' \
    -v ended="$(run_ended && echo 1 || echo 0)" '
    FILENAME == ARGV[1] { if ($0 != "") isMissing[$0] = 1; next }
    FILENAME == ARGV[2] { if ($0 != "") wasStarted[$0] = 1; next }
    FILENAME == ARGV[3] { if ($0 != "") isRunning[$0] = 1; next }
    {
      name = $1; drv = $2; out = $3;
      if (isRunning[drv])        state = "building";
      else if (!isMissing[out])  state = wasStarted[drv] ? "built" : "cached";
      else if (wasStarted[drv])  state = "FAILED";
      else if (ended == 1)       state = "FAILED";
      else                       state = "queued";
      printf "%s\t%s\n", state, name;
    }' "$invalid" "$started" "$running" "$plan"
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

  if [ -s "$summary" ]; then
    printf '\n'
    cat "$summary"
  fi
}

# One line, for the terminal the run itself is in. A gate that
# prints nothing until it ends is not observable in the place a
# reader is already looking, whatever `gate status` can answer
# from somewhere else.
progress() {
  local elapsed=$1
  summarize | awk -F'\t' -v elapsed="$elapsed" '
    {
      count[$1]++;
      if ($1 == "building") building = building (building ? " " : "") $2;
    }
    END {
      printf "[%3d:%02d] built %d  building %d  queued %d  failed %d  |  %s\n",
        elapsed / 60, elapsed % 60,
        count["built"], count["building"], count["queued"],
        count["FAILED"], building;
    }'
}

# `nix log` wants a derivation, and a caller has a check name.
show_log() {
  local name=$1 drv
  drv="$(awk -F'\t' -v n="$name" '$1 == n { print $2 }' "$plan")"
  if [ -z "$drv" ]; then
    die "No check is named $name in this run." \
        "List the names with: nix run .#gate -- status"
  fi
  nix log "$drv"
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
  rm -f "$summary" "$plan" "$meta" "$folded" "$cursor"

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
  # terminal gets the progress line instead. A reader watching the
  # run sees it here, and a reader anywhere else runs
  # `gate status` against the same state.
  #
  # One line goes out before the first wait, so a run is never silent
  # at the start, and the poll is shorter than the line so that a run
  # ending between two lines is noticed when it ends. Waiting a whole
  # print interval to notice put a floor under every run, including
  # one the store already held.
  local waited=0
  progress 0
  while kill -0 "$build_pid" 2>/dev/null; do
    sleep "$poll_seconds"
    waited=$((waited + poll_seconds))
    if [ "$waited" -ge "$progress_seconds" ]; then
      waited=0
      progress "$(( $(date +%s) - start ))"
    fi
  done
  wait "$build_pid" || rc=$?

  # Counted before the summary is written, because writing it is what
  # makes a run read as ended, and an ended run reports the checks that
  # never started as failed too. What is wanted here is the narrower
  # number: the checks that started and did not finish.
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
