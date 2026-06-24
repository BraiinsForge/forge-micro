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

The authoritative gate is a single command, run identically to CI so the
local result cannot disagree with the pipeline:

```
nix flake check .
```

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
- `rust-toolchain.toml` channels match the target architecture

### Phase 6: Verification checklist

Before producing output, verify every category was checked. For each item below, confirm it was evaluated for every commit and every changed file. If any item was skipped or only partially checked, go back and complete the relevant phase before continuing.

- [ ] All checks ran (Phase 2)
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
nix flake check . (all projects + formatting): PASS/FAIL

### Findings

BLOCKING file.rs:42 -- Description of the issue
BLOCKING file.rs:58 -- Description of the issue

NIT file.rs:100 -- Description of the issue
```

Severity levels:
- `BLOCKING` — Must fix before merge: bugs, build failures, lint errors, format violations, rule violations, incorrect commit messages
- `NIT` — Optional improvement, style preference, take it or leave it

If no issues are found, output: `No issues found. Branch is merge-ready.`

## Rules

- Review ALL commits on the branch, not just the latest
- Run `nix flake check .` (repo root) every review — it is the entire CI check stage; never skip it or swap in a narrower command
- Every finding MUST include a file:line reference (or commit hash for commit message issues)
- Findings must be exhaustive: if this review passes, the branch is ready to merge
- No praise, no "looks good" summaries, no filler text
- No suggestions without file:line references
- Report only concrete issues found in the actual code or checks
- Do not invent issues that are not evidenced by code, diffs, or check output
- Use `Task` agents for heavy exploration to save context
