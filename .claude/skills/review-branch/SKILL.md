---
name: review-branch
description: Deep review of all branch changes against the review base (origin/master, or a stacked base via branch.<name>.reviewBase) — code, architecture, commits, and CI readiness
context: fork
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

1. `git log --oneline "$BASE"..HEAD` — the commit list
2. `git diff --stat "$BASE"...HEAD` — the cumulative shape
3. `git log -p --submodule=short "$BASE"..HEAD` — **the one full read of
   the branch.** Every commit's message followed by its own patch, in a
   single output. This is the branch input to Phases 3 through 5; none
   of them reads the branch again, though Phase 5 additionally reads
   the architecture docs its reference routes to. There is no separate
   full `git diff` and no per-commit `git show`.

   `--submodule=short` because a consumer may set `diff.submodule=log`,
   which expands a single gitlink change into the whole submodule
   history and buries the branch in output belonging to another repo.

   Per-commit patches are what Phase 3 needs and what a cumulative diff
   cannot supply: `..` gives each commit's own patch, `...` gives the
   diff against the merge base, and the two coincide only on a linear
   branch. Step 2 is the cheap cross-check — if the per-commit patches do
   not account for the cumulative stat, the branch carries a merge or a
   rebase artefact, which is itself a finding.
4. For non-trivial changes, read full files for context beyond the diff

### Phase 2: Run all checks

If the caller states the gate has already been run for this tree and
gives its result, record that result and go to Phase 3. Run no `nix` and
no `cargo`, and report no BUILD findings — they belong to whoever ran the
gate. A caller that gates several trees in one invocation does this, and
it owns the whole of the reasoning below.

Otherwise the gate is this phase's to run. First decide whether the flake
check applies. The flake checks compile
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

When `FLAKE_RELEVANT=1`, the gate is `nix flake check .` at the repo root
and nothing narrower. It is the entire CI `check` stage, it must run as
one foreground blocking `Bash` call, and the exit line must be in hand
before the report. How to run it, the three ways of running it that have
failed, how to read the result, and what must never be substituted for it
are in [reference/flake-gate.md](reference/flake-gate.md) — read that
before running the gate.

### Phase 3: Per-commit review

First run the format linter, exactly as CI's lint stage runs it:

```bash
nix shell nixpkgs#gitlint --command gitlint --commits "$BASE..HEAD" 2>&1; echo "GITLINT_EXIT=$?"
```

gitlint (`.gitlint` + `.gitlint-rules/`) settles the mechanical format:
the title regex and length, the body line shape, trailers and
signatures, the banned prefixes, and issue-tag placement. Each line it
prints is a COMMITS finding quoting that line. Do not re-derive its
rules by hand, and do not report a format violation it did not print —
the rule set loads from the checkout, so what it enforces is the
branch's own configuration, and hand-checking its domain is how a
review disagrees with the pipeline.

Then work from the Phase 1 step 3 output — every commit's message and
its own patch are already there; do not re-read the branch — and judge
what no linter can:

- Title is imperative and a high-level what, not a file list
- Body explains WHY the change was needed, not WHAT changed in the code
- Body does NOT enumerate code changes the reviewer can see in the diff
- One logical change per commit
- Lock files not bundled with source changes
- AI/tooling config not bundled with code changes

**Diff-vs-body verification:**
- Every claim in the commit body matches the actual diff
- No diff content missing from the body description
- No body claims that are not evidenced by the diff

### Phase 4: Code correctness

Judge the Phase 1 read for what no gate can decide. Clippy under
`-D warnings`, `cargo fmt`, `nixfmt` and `cargo-machete` already settle
formatting, needless clones, unused imports, dead code and unused
dependencies. Those are BUILD findings when they fire, and not this
phase's work when they do not — do not hand-check the gate's domain.

**Logic and safety:**
- Logic errors, off-by-one, race conditions
- Missing error handling at system boundaries
- Security: injection, unsafe without justification, secrets in code
- Edge cases and failure modes

**Rust:**
- No `unwrap()` or `expect()` in production code where error handling is appropriate
- Proper use of `Result` and `Option`
- Correct lifetime annotations
- Async tasks do not block

**Embedded firmware (when applicable):**
- No heap allocation (`alloc`, `Vec`, `String`, `Box`)
- No `unwrap()`, `expect()`, `panic!()` outside tests
- Peripheral access follows ownership model (move semantics)
- Embassy task spawning uses correct static lifetimes
- PubSub channel dimensions match actual publisher/subscriber count
- Integer arithmetic for MCUs without FPU
- Cooperative watchdog implementation

### Phase 5: Architecture compliance

Judge the diff against the architecture documented in `docs/`
(`ubos-main/docs/` when reviewing the consumer repo). Which docs the
touched paths route to, what is normative versus historical, the
violation classes to hunt, and the doc-drift check are in
[reference/architecture-gate.md](reference/architecture-gate.md) — read
that before judging. When no crate changed, record
`architecture gate: SKIPPED (no crate changed)` and move on.

Findings from this phase are `ARCH`: a dependency edge against the
layer map, cross-module traffic outside the communication contract, a
module off the uniform template, I/O or policy on the wrong side of a
documented seam, and behaviour changes that leave a routed doc stale.

### Phase 6: Style and cross-cutting

**CLAUDE.md compliance:**
- Comments are proper sentences, no parenthetical asides, no size claims
- Code follows existing patterns in the codebase
- Commented-out code, and branches no caller can reach
- Duplication: same content defined in multiple places
- No stale references after renames

**Integration:**
- New files/modules properly integrated (imports, mod declarations)
- Nix flakes reference correct paths after any restructuring
- `.cargo/config.toml` runner and target settings correct
- The `nix/toolchains.nix` targets match the target architecture

### Phase 7: Verification checklist

Before producing output, verify every category was checked. For each item below, confirm it was evaluated for every commit and every changed file. If any item was skipped or only partially checked, go back and complete the relevant phase before continuing.

- [ ] Gate result in hand — an own `FLAKE_CHECK_EXIT=` line, a caller-supplied result, or a recorded skip or timeout (Phase 2)
- [ ] The branch was read once, in Phase 1 step 3, and not re-read since
- [ ] gitlint run with its `GITLINT_EXIT=` line in hand (Phase 3)
- [ ] Every commit message verified against its diff (Phase 3)
- [ ] Every commit is a single logical change (Phase 3)
- [ ] No bundled unrelated changes (Phase 3)
- [ ] Logic errors and edge cases checked (Phase 4)
- [ ] Security reviewed (Phase 4)
- [ ] Rust patterns verified (Phase 4)
- [ ] Architecture judged against the docs the diff routes to, or the skip recorded (Phase 5)
- [ ] Doc drift checked: behaviour changes still match the docs describing them (Phase 5)
- [ ] CLAUDE.md style rules checked (Phase 6)
- [ ] Cross-cutting integration verified (Phase 6)
- [ ] No stale references from renames (Phase 6)

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
ARCH     src/pid.rs:88 -- cooling writes mining.halt directly; only the manager writes halt/resume (ARCHITECTURE.md, communication rules)
BUILD    src/bar.rs:10 -- clippy: needless clone (-D warnings)
DOCS     CLAUDE.md:88 -- stale flag in build recipe
COMMENTS src/x.rs:5 -- comment contradicts code
COMMITS  a1b2c3d -- title exceeds 72 chars (gitlint UC rule)
```

Categories — label each finding by the domain it lives in. Every finding
is important and must be addressed; there is no severity ranking:
- `CODE` — source logic, correctness, runtime behavior
- `ARCH` — violations of the architecture in `docs/`, and behaviour changes that leave those docs stale; each cites the doc and section
- `BUILD` — CI-gate failures: build errors, clippy under `-D warnings`, `cargo fmt`/nixfmt violations
- `DOCS` — documentation, markdown, `CLAUDE.md`, skill files, rustdoc
- `COMMENTS` — in-code comments that are wrong, stale, or misleading
- `COMMITS` — commit message format (gitlint rules), history structure, atomicity

If no issues are found, output: `No issues found. Branch is merge-ready.`

## Rules

- Review ALL commits on the branch, not just the latest
- Run `nix flake check .` (repo root) as one foreground, blocking `Bash` call whenever the diff touches flake inputs — it is the entire CI check stage; never swap in a narrower command. Skip it only when Phase 2 detection shows an all-inert diff, or when the caller supplied the gate result, and record which in the output
- Read the branch exactly once, with the Phase 1 step 3 `git log -p`. Later phases judge that output; they do not run their own `git show` or `git diff`
- Every finding MUST include a file:line reference (or commit hash for commit message issues)
- Findings must be exhaustive: if this review passes, the branch is ready to merge
- No praise, no "looks good" summaries, no filler text
- No suggestions without file:line references
- Report only concrete issues found in the actual code or checks
- Do not invent issues that are not evidenced by code, diffs, or check output
- The Phase 1 read is normally the whole input. Reach for a `Task` agent only when the branch is too large to hold at once, and never for work a phase above already has in context — a fresh agent re-reads `CLAUDE.md` before it can help
