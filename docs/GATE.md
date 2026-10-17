# The gate

`nix run .#gate` builds every check of every tree in the checkout. It
prints one table. The mechanism is `nix/gate.nix` and `nix/gate.sh`.

This repository can stand alone, and it can be the inner half of a pair
with a private repository that consumes it. The gate is the same
mechanism in both cases. It finds the outer repository first, so a run
started here gates every tree of the checkout.

## The commands

Run the gate:

```sh
nix run .#gate
```

Print the table of the newest run:

```sh
nix run .#gate -- status
```

Print the build log of one check:

```sh
nix run .#gate -- log <check>
```

The gate app belongs to this repository, and the commands above name it
from this repository's root. If this repository is the inner half of a
pair, you do not have to go there. Name the app by its path instead:

```sh
nix run ./ubos-main#gate
```

The two references are the same app, and each one gates every tree of
the checkout. Use the one that is correct from the directory you are in.
The messages that the gate prints name the first reference, because the
gate cannot know which directory you started it from.

Two things can stand around the command. `GATE_MAX_JOBS=<number>` and
`GATE_CORES=<number>` go before `nix run`. By default, the gate builds
as many derivations at the same time as the machine has cores, and each
derivation uses two cores. If the best pair of values for your machine
is different, use them. A `2>&1` goes after the command. It is a
redirection and not a pipe, so the exit status stays the gate's own.

CAUTION: Do not add a pipe, a redirect to a file, or a trailing
ampersand. A pipe keeps all of the output until the run stops. It also
makes the exit status the last command's and not the gate's. A trailing
ampersand keeps neither, but it is refused with them, because the gate
is held to one spelling for each root. The hook that refuses all three
is `.claude/hooks/gate-invocation`, and it refuses them before the
command reaches a shell.

To start the gate and continue to work, run it in the background. Use
the background mode of the tool that runs the command. Do not use a
trailing ampersand. Then read the table later with `-- status`. The
state of a run is per worktree, and `git status` does not see it.

The state stays on disk after the run ends, because `-- status` reads it
later. It holds a copy of the tracked content of every tree, which is
about the size of the checkout, and the event stream of the run, which
can be tens of megabytes. The next run in the same worktree writes over
both. To remove the state, remove the `gate` directory in the git
directory of the worktree.

## What the run refuses

If one of these conditions is true, the run stops before it starts. Each
refusal names the condition and the command that clears it.

1. A submodule of the checkout is not checked out.
2. The outer tree has uncommitted changes.
3. A submodule tree has uncommitted changes.
4. A submodule is not checked out at the commit that its gitlink names.
5. No tree reported a check to build.
6. A tree did not evaluate. This covers its checks and its dev shells.
7. A gate is already running for this worktree.

The gate judges the committed tree. It reads the tracked content of each
tree into its own directory first, and that content is what a merge
carries. For this reason a dirty tree is refused and not gated.

Condition 5 is not reachable from a tree of this checkout. The run
refuses it because `nix build` with no target builds the default package
of the current directory. That result is a green gate over something
that nobody asked for.

## How to read the table

The table gives one row to each check that is building or that failed.
Under the rows it gives a count of every state: `cached`, `built`,
`building`, `queued` and `failed`. A check that is not building and did
not fail has no row of its own. The counts are where it is accounted
for.

A check is named as failed at the moment it fails, and not at the end of
the run. The gate always builds with `--keep-going`, so one failed check
does not stop the others. A table that you read in the middle of a run
tells you what is already broken.

There is no table during the evaluation, which is the phase before the
build. In that phase cargo plans the host and ARM checks, one derivation
per compile step, which is why the gate passes nix the option that
allows a program to run during evaluation; `nix/units.nix` holds the
reason. The gate counts no check until the evaluation ends. If you ask
for the status in that time, `-- status` tells you that the run is in
the evaluation. This is not an error, and you must not start a second
run: the gate refuses one for the same worktree.

The last line is the verdict, `gate: PASS` or `gate: FAIL`. Lead with
it. If the verdict is `gate: FAIL`, name the failed checks from the
table. Then read one log with `nix run .#gate -- log <check>`.

## What the gate covers

The gate builds the `checks` of every tree. It also evaluates the dev
shells of every tree. Against `nix flake check`, one output is missing:
`nix flake check` also forces `nixosConfigurations`, and the gate does
not.

A tree that carries a NixOS system must gate that system from a check of
its own. This is the same rule that a `packages` output follows. If the
tree does not, the system is gated nowhere.
