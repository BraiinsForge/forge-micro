---
name: fix-review
description: Applies the findings of a review as fixup commits against the branch commits they belong to. Use after /review-branch.
allowed-tools: Bash, Read, Edit, Write, Grep, Glob, Skill
---

Apply the findings in `$ARGUMENTS`, and attribute each fix to the
branch commit it corrects.

The judgement is the built-in review's and is not repeated here. This
skill owns one thing the review does not: a fix to a line an earlier
branch commit introduced lands as a `fixup!` of that commit, so that
`/branch-cleanup` folds it and the branch stays one logical change per
commit.

## Process

1. Resolve the base, the same way `/review-branch` does:

   ```bash
   git rev-parse "$(git config "branch.$(git branch --show-current).reviewBase" || echo origin/master)"
   ```

   Spell the hash it prints out where step 4 names `<base>`. Each tool
   call runs a shell of its own, so a variable set here is gone by
   then, and absorb would be handed an empty base and fail to resolve
   it.

   Refuse to start on a dirty tree: a fix has to be told apart from
   whatever else is in it, and absorb cannot.

2. Sort each finding in `$ARGUMENTS` by severity first, and let the
   severity decide what happens to it:

   | Severity | What it is | What this pass does |
   |----------|------------|---------------------|
   | important | would break behaviour, leak data, or fail the pipeline | fix it |
   | nit | true, small, and not in the way | fix only where the file is already being edited for an important finding; otherwise count it in the report |
   | pre-existing | real, and not introduced by this branch | drop it, named in the report |

   The branch is done when no important finding is left, not when a
   round reports nothing. A review always has something to say about
   code it is reading for the second time, so a pass that treats every
   finding as blocking never ends; that is what the severities are for.
   A pre-existing bug is its own branch, and a nit that survives the
   branch is a nit the next reader can raise.

   Apply what the table keeps to the working tree, by hand. A finding
   that is wrong is answered in the report below, not with a doc change
   written to forestall it.

   A finding against a markdown file is rejected unless it names a
   command that fails, a path that does not exist, or a contradiction
   inside the one file. An `ARCH` doc-drift finding is the other
   exception: it names a contradiction between the code and the doc
   that describes it, the review withholds `Merge-ready.` while it
   stands, and the fix brings the stale doc to the behaviour the
   branch changed, as
   `../review-branch/reference/architecture-gate.md` prescribes.
   Wording, a sentence that another file states
   differently, or a rule the finder would have written another way,
   is `rejected: prose` in the report and stays as it is. A prose fix
   is what the next round reads, so accepting one is what keeps the
   loop going.

3. Verify: `cargo check` in every affected crate.

4. Attribute. The tree holds nothing but the fix, so stage it whole
   and let absorb find the owning commits:

   ```bash
   git add -A
   nix develop .#arm -c git absorb --base <base> --dry-run
   nix develop .#arm -c git absorb --base <base>
   ```

   `git absorb` is a dev-shell tool and is absent from a plain session
   shell, so it runs under a dev shell that carries it.

   Absorb creates one `fixup!` per owning commit and leaves in the
   index what no branch commit owns: a new file, or a line the branch
   never touched. It reads the index and commits only what it can
   attribute, so the rest stays staged rather than becoming unstaged.
   What it leaves is committed on its own with `/commit`; it is a
   change to the base, not a fix of the branch.

   For a single fix whose owner is known, `git commit --fixup=<hash>`
   is the same result without absorb.

5. A finding that needs a commit boundary moved is a structural fix.
   Do it with the recipes in
   `../branch-cleanup/reference/rebase-ops.md`, which bind unchanged,
   `GIT_SEQUENCE_EDITOR` included. So do the rules of
   `../branch-cleanup/SKILL.md` around them: a backup branch before
   the rebase, and `git rebase --abort` on a conflict rather than a
   guess.

## Report

```
## Fixes Applied

fixup! <target title> -- <what was fixed>
standalone: <title> -- <why no branch commit owns it>
rejected: <finding> -- <why it is wrong>
dropped: <finding> -- pre-existing, not this branch
nits: <N> not fixed -- <one line naming what they were about>
```

## Rules

- Every finding is accounted for in the report: fixed, rejected,
  dropped as pre-existing, or counted among the nits.
- Severity decides the action, per step 2. Only an important finding
  has to be fixed before the branch merges.
- A prose finding is rejected by default, per step 2.
- One finding per fixup commit.
- A fixup carries nothing beyond its finding.
