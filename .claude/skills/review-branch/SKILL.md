---
name: review-branch
description: Deep review of all branch changes against the review base (origin/master, or a stacked base via branch.<name>.reviewBase) — code, commits, and CI readiness
context: fork
disable-model-invocation: true
allowed-tools: Bash, Read, Grep, Glob, Skill, Task, WebSearch, WebFetch
---

Deep, exhaustive review of all commits on the current branch compared to its review base (`origin/master` by default, or a stacked base configured via `branch.<name>.reviewBase`; see Phase 1). The goal is that after all findings are fixed, the branch is merge-ready and all pipeline checks will pass. Do not leave anything for a second pass — find everything in one review.

## Process

### Phase 1: Gather context

0. Resolve the review base. The branch may be stacked on another branch
   rather than `master`, so do not assume `origin/master`:

   ```bash
   BASE=$(git config "branch.$(git branch --show-current).reviewBase" || echo origin/master)
   ```

   Report which base was resolved. To stack a branch, set its base once:
   `git config branch.<name>.reviewBase <base-ref>`.

1. `git log --oneline "$BASE"..HEAD` — list all commits
2. `git diff "$BASE"...HEAD --stat` — see which files changed
3. `git diff "$BASE"...HEAD` — full diff of all changes
4. For non-trivial changes, read full files for context beyond the diff

### Phase 2: Run all checks

First decide whether the flake check applies. The flake checks compile
firmware and run host tests, DRC, and ERC; a diff that changes only
documentation or harness config (`.claude/`, `*.md`, `.gitlint*`) feeds
into none of them, so running the check would rebuild every board
without validating any of the change under review.

```bash
FLAKE_RELEVANT=0
while IFS= read -r f; do
  case "$f" in
    .claude/*|*.md|.gitlint*) ;;
    *) FLAKE_RELEVANT=1; break ;;
  esac
done < <(git diff --name-only "$BASE"...HEAD)
```

The loop flags the diff as relevant when any changed file falls outside
the inert set, so any unexpected path forces the check on; only an
all-inert diff skips it. The test is plain shell on purpose. Do not
rewrite it around `grep`: the host grep may be ugrep, whose `-q -v`
exits 1 whenever any line matches the pattern even though non-matching
lines exist, which silently skipped this gate on mixed diffs. When
`FLAKE_RELEVANT=0`, skip the check and record
`nix flake check .: SKIPPED (no flake inputs changed)` in the output,
then move to Phase 3.

When `FLAKE_RELEVANT=1`, the authoritative gate is a single command, run
identically to CI so the local result cannot disagree with the pipeline:

```
nix flake check .
```

This is a full hermetic build of every board. Warm — most reviews, since
the local `/nix/store` is already populated — it takes a minute or two;
cold it can run much longer.

Run it as **one foreground, blocking `Bash` call** with a long `timeout`
(the 600000 ms maximum). Do not background it, do not delegate it to a
`Task` subagent, and do not poll a log for a completion sentinel. Each has
failed in a way rooted in this skill's forked context (`context: fork`): a
fork is one-shot, so once it returns its final message it cannot be
re-entered. A backgrounded check's completion notification then lands in
the main loop, never in the finished fork, so the report gate below is
unreachable and the review ends with no report; a `Task` subagent likewise
returns before the check finishes; and a hand-rolled
`until grep …; do sleep; done` waiter spins whenever its sentinel never
lands. A foreground call sidesteps all three: it blocks this same turn
until nix exits, so the exit status is in hand before the call returns.
`2>&1` is a redirection, not a pipe, so `$?` is nix's own exit status;
capture it on the last line:

```bash
nix flake check . 2>&1; echo "FLAKE_CHECK_EXIT=$?"
```

Read the last line of the output: PASS if it is `FLAKE_CHECK_EXIT=0`,
otherwise FAIL with each failing check name and the relevant error lines.
If the call hits its timeout before the exit line appears, the store was
cold; record the flake check as `TIMED OUT (re-run manually)` and note
that the branch is not gated until it is re-run, rather than dropping the
report. Do not emit the final report until this exit line is in hand or
the timeout is recorded.

The repo-root flake aggregates every project's checks (clippy, firmware
builds, host tests, DRC, ERC) under a `<project>-` prefix, plus the
repo-wide treefmt formatting check. This is the entire CI `check` stage
(`.gitlab/ci/lint.yml`), so one command covers what the pipeline gates.
Do not run per-flake `nix flake check <path>`; the root check already
includes them, and the root is the only place the nix-formatting check
lives.

Never substitute an ad-hoc `nixfmt`, `treefmt`, or per-crate
`cargo fmt --check`. Only the flake check uses the formatter version
pinned in `flake.lock`, so only it matches CI exactly; a locally
installed formatter can be a different version and pass while CI fails.

`cargo check` / `cargo clippy` per changed crate are fine for fast
iteration, but a clean run does not substitute for the flake check above.

The flake checks are hermetic: every toolchain — including the Xtensa
toolchain the `.#esp` shell uses — is supplied by the flake and built by
nix, so `nix flake check` runs every board's checks with no host setup,
the same way CI does. There is no "missing toolchain" to skip over; if a
check does not run, the environment is wrong — fix it and run the check.

### Phase 3: Per-commit review

For each commit, verify against CLAUDE.md commit format rules:
- Title: module prefix, imperative verb, high-level summary
- Body explains WHY the change was needed, not WHAT changed in the code
- Body does NOT enumerate code changes the reviewer can see in the diff
- One logical change per commit
- Lock files not bundled with source changes
- AI/tooling config not bundled with code changes
- No Co-Authored-By, no AI signatures, no emojis

**Diff-vs-body verification:**
- Every claim in the commit body matches the actual diff
- No diff content missing from the body description
- No body claims that are not evidenced by the diff

### Phase 4: Code correctness

Review the full diff (`git diff "$BASE"...HEAD`) for:

**Logic and safety:**
- Logic errors, off-by-one, race conditions
- Missing error handling at system boundaries
- Security: injection, unsafe without justification, secrets in code
- Edge cases and failure modes

**Rust-specific:**
- No `unwrap()` or `expect()` in production code where error handling is appropriate
- Proper use of `Result` and `Option`
- No unnecessary allocations or clones
- Correct lifetime annotations
- Async tasks do not block

**Embedded firmware (when applicable):**
- No heap allocation (`alloc`, `Vec`, `String`, `Box`)
- No `unwrap()`, `expect()`, `panic!()` outside tests
- No blocking operations in async tasks
- Peripheral access follows ownership model (move semantics)
- Embassy task spawning uses correct static lifetimes
- PubSub channel dimensions match actual publisher/subscriber count
- Integer arithmetic for MCUs without FPU
- Cooperative watchdog implementation

### Phase 5: Style and cross-cutting

**CLAUDE.md compliance:**
- Comments are proper sentences, no parenthetical asides, no size claims
- Code follows existing patterns in the codebase
- Dead code: unused imports, unreachable branches, commented-out code
- Duplication: same content defined in multiple places
- No stale references after renames

**Integration:**
- New files/modules properly integrated (imports, mod declarations)
- Nix flakes reference correct paths after any restructuring
- `.cargo/config.toml` runner and target settings correct
- The `nix/toolchains.nix` targets match the target architecture

### Phase 6: Verification checklist

Before producing output, verify every category was checked. For each item below, confirm it was evaluated for every commit and every changed file. If any item was skipped or only partially checked, go back and complete the relevant phase before continuing.

- [ ] Flake check exit line (`FLAKE_CHECK_EXIT=`) in hand, or was skipped/timed-out with reason recorded (Phase 2)
- [ ] Every commit message verified against its diff (Phase 3)
- [ ] Every commit is a single logical change (Phase 3)
- [ ] No bundled unrelated changes (Phase 3)
- [ ] Logic errors and edge cases checked (Phase 4)
- [ ] Security reviewed (Phase 4)
- [ ] Rust patterns verified (Phase 4)
- [ ] CLAUDE.md style rules checked (Phase 5)
- [ ] Cross-cutting integration verified (Phase 5)
- [ ] No stale references from renames (Phase 5)

Only produce the final output after all items are confirmed.

## Output Format

```
## Review: <branch-name> (<N> commits)

### Check Results

cargo check <crate>: PASS/FAIL
cargo clippy <crate>: PASS/FAIL
nix flake check . (all projects + formatting): PASS/FAIL/SKIPPED

### Findings

CODE     src/foo.rs:42 -- off-by-one in length check
BUILD    src/bar.rs:10 -- clippy: needless clone (-D warnings)
DOCS     CLAUDE.md:88 -- stale flag in build recipe
COMMENTS src/x.rs:5 -- comment contradicts code
COMMITS  a1b2c3d -- title exceeds 72 chars (gitlint UC rule)
```

Categories — label each finding by the domain it lives in. Every finding
is important and must be addressed; there is no severity ranking:
- `CODE` — source logic, correctness, runtime behavior
- `BUILD` — CI-gate failures: build errors, clippy under `-D warnings`, `cargo fmt`/nixfmt violations
- `DOCS` — documentation, markdown, `CLAUDE.md`, skill files, rustdoc
- `COMMENTS` — in-code comments that are wrong, stale, or misleading
- `COMMITS` — commit message format (gitlint rules), history structure, atomicity

If no issues are found, output: `No issues found. Branch is merge-ready.`

## Rules

- Review ALL commits on the branch, not just the latest
- Run `nix flake check .` (repo root) as one foreground, blocking `Bash` call whenever the diff touches flake inputs — it is the entire CI check stage; never swap in a narrower command. Skip it only when Phase 2 detection shows an all-inert diff, and record the skip in the output
- Every finding MUST include a file:line reference (or commit hash for commit message issues)
- Findings must be exhaustive: if this review passes, the branch is ready to merge
- No praise, no "looks good" summaries, no filler text
- No suggestions without file:line references
- Report only concrete issues found in the actual code or checks
- Do not invent issues that are not evidenced by code, diffs, or check output
- Use `Task` agents for heavy exploration to save context
