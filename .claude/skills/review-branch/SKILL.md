---
name: review-branch
description: Review the branch against its review base (origin/master, or a stacked base via branch.<name>.reviewBase) — the flake gate, gitlint, the built-in code review, and the architecture agent when a crate changed
allowed-tools: Bash, Read, Grep, Glob, Skill, Task
---

Review the branch and report once.

The judgement of the code is the built-in `/code-review`. This skill
adds only what that review does not run: the flake gate, gitlint, and
the architecture check against `docs/`. It reads no diff of its own.

## Process

1. Resolve the base. It is not the local `master`, which is often
   stale, and the branch may be stacked:

   ```bash
   git rev-parse "$(git config "branch.$(git branch --show-current).reviewBase" || echo origin/master)"
   ```

   Refuse to start on a dirty tree: the gate judges the committed
   tree and a review of uncommitted work cannot be fixed by commit.

   Every command below spells the resolved value out where this block
   names `$BASE`. Each tool call runs its own shell, so a variable set
   here is gone by the next call, and the command then runs on an empty
   range: gitlint reads no commit and reports nothing, and the crate
   test in step 5 names no file and skips the architecture check.

2. Start the gate in the background, in the same message as step 3's
   review invocation:

   ```bash
   nix run .#gate
   ```

   Step 3's resolution block therefore runs before this one, in a
   message of its own: a Bash call is backgrounded whole or run in the
   foreground whole, so the block whose output step 3 has to read
   cannot share a message with the gate.

   Its completion notification is the whole of the wait. Never poll:
   no `sleep` loop, no re-reading its output file. `nix run .#gate --
   status` reprints the newest run's table for a later turn. A caller
   that already runs the gate for a pair of trees says so, and then
   this step is skipped.

3. Resolve what the review sees. The range is `<BASE>...HEAD`, every
   time: the review is deterministic, so the same branch against the
   same base is judged the same way on every run, and nothing narrows
   the range or skips the round.

   ```bash
   git rev-parse <BASE> HEAD
   ```

   Invoke the built-in review through the Skill tool, in the same
   message as the gate, with both ends of the range as full hashes
   from `git rev-parse`:

   ```
   code-review medium <from>...<to>
   ```

   `medium` returns few, high-confidence findings; that is the point.
   Pass a higher level only when the user asks for one.

4. Run gitlint and keep its exit line:

   ```bash
   nix run .#gitlint 2>&1; echo "GITLINT_EXIT=$?"
   ```

   The app is `nix/gitlint.nix`. It resolves the same base step 1
   does, runs the linter this flake pins over `<BASE>..HEAD`, and
   prints one line per tree of the checkout. Its range is the one
   CI's own `commits` job judges, so a stacked base narrows it and
   nothing else does.

   Every line it prints past the tree's own is a finding quoting that
   line. Do not check commit format by hand: the rule set loads from
   the checkout, and a hand check is how a review disagrees with the
   pipeline.

5. When the range touches a crate, which is any `.rs` file or any
   `Cargo.toml` in `git diff --name-only <BASE>...HEAD`, spawn one
   agent over `<BASE>...HEAD`, `model: "opus"`,
   told to read `reference/architecture-gate.md` first and then the
   docs that file routes to, and take its verdict. A checkout that
   defines an `arch-review` agent uses that one.
   A range that touches no crate records
   `architecture: SKIPPED (no crate changed)`.

Do not emit the report until the gate has its exit line, the review has
returned, and the agent, when spawned, has returned. A result that lands
after the turn ends re-enters this session, and the report is emitted
from the turn the last one lands in.

## Output Format

```
## Review: <branch> (<N> commits against <BASE>)

gate: PASS | FAIL <failing checks> | BLOCKED <why> | RUN BY CALLER
gitlint: PASS | <its lines>
architecture: PASS | SKIPPED (no crate changed) | <ARCH findings>
review: <from>...<to>

### Findings

#### Important
<LABEL> <file:line> -- <finding>

#### Nit
<LABEL> <file:line> -- <finding>

#### Pre-existing
<LABEL> <file:line> -- <finding>
```

A heading with nothing under it is omitted. The label is the domain
the finding lives in: `CODE` for logic and runtime behaviour, `DESIGN`
for structure and cost that grows, `ARCH` for the architecture in
`docs/`, `BUILD` for what a gate would fail, `DOCS` for prose,
`COMMENTS` for a comment that contradicts its code, `COMMITS` for
history. The severity decides what `/fix-review` does with a finding;
the label tells the reader where to look.

Close with `Merge-ready.` when the gate passes, gitlint prints nothing,
the architecture check has no finding, and the review reports no
**important** one. A branch that reports no finding at all is not the
bar and is not reachable: a review reading code a second time always
has something to say, so the bar is the severity, which `/fix-review`
owns. A finding is fixed with `/fix-review`, which attributes each fix
to the branch commit it corrects; `/branch-cleanup` then folds the
fixups.

Report the tally beside the verdict, so a reader sees the shape before
the detail: `3 important, 6 nit, 1 pre-existing`, and lead with
`no important findings` when that is the case.

The skill keeps no count of rounds and no memory of what a round saw.
When to stop re-reviewing is the user's call, made with open eyes: a
review that re-reads settled code always has something to say, and
past a point what it finds is its own wording.

## What the review is told

The built-in review runs as a fork of this session and reads this file
with it, so the rules it judges by are these:

- **Important** is a finding that would break behaviour, leak data, or
  fail the pipeline. Everything else is a nit at most. A branch merges
  on zero important findings, never on zero findings.
- A bug this branch did not introduce is **pre-existing**. Say so and
  do not count it against the branch; it is its own branch.
- Report at most five nits. Past that, give a count and one line
  saying what they were about.
- Do not report what a gate already enforces: formatting, commit
  message format, a dead path in an instruction file, the line budget
  of `CLAUDE.md`.
- A claim about behaviour needs a `file:line` that shows it, not an
  inference from a name.

## Rules

- Do not read the diff to judge it. The review does that once; a second
  reader beside it doubles the findings and settles nothing.
- Do not substitute for a gate that cannot run. Report it `BLOCKED` with
  the reason.
- Never push.
