---
paths:
  - "docs/**/*.md"
  - "**/README.md"
  - "**/CONTRIBUTING.md"
---

# Prose

New user-facing prose follows `/simple-english`. Read that doctrine before
writing or revising a file under this rule, and follow it. It is the
authority; nothing in it is restated here.

The rule loads with the files it governs, so it does not wait to be asked
for. A doc, a README, a runbook or an operational note is in scope the
moment its meaning changes; prose left alone is left alone.

Out of scope, and governed elsewhere: commit messages by `.gitlint` and
the `/commit` doctrine, code and nix comments by `.claude/rules/comments.md`,
and agent doctrine under `.claude/` by nothing but itself.
Those rules contradict this one in places, on semicolons and parentheses
among others, and that is deliberate.

An error message is prose too, but it lives in a `.rs` or `.nix` file that
no path here can select without pulling in the whole codebase. `CLAUDE.md`
carries that half of the rule.
