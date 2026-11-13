---
paths:
  - "**/*.nix"
  - "**/*.rs"
---

# Comments

A comment states a reason the code cannot state: an invariant, a
hardware or protocol fact, a bound and what fixes it, a constraint the
next edit must keep. Everything else is deleted.

- **What, then why.** A doc comment on a function, block, module or
  type says what it does in one sentence, then why it exists. It never
  says what the thing does not do or what it is not.
- **Once, at the owning site.** A reason that covers a module goes in
  the module doc. A reason that belongs to one item goes on that item.
  A second site that needs it points there: "see `Flash::CONFIG_RANGE`".
  It is never written out again, in a sibling crate or in a test.
- **The conclusion, not the search.** State the fact that holds and the
  one reason it holds. Not what was tried, not what the old value was,
  not what broke before, not what an earlier comment said. A story that
  is still useful moves to a file under `docs/`. Most are not useful.
- **No alternatives.** A comment does not list the designs that were
  not chosen or the work that is deferred. A deferred change is an
  issue, not a comment.
- **No restatement.** A doc that says the item's name in words is
  deleted. A field doc that repeats the field name is deleted. A test
  function carries no doc comment: a reason inside one demotes to a
  plain `//` comment in place, a restatement is deleted outright. A
  comment above a line that reads the same without it is deleted.
- **Two sentences.** A reason that needs more is written as the bound
  it protects plus the fact behind it, and the rest is cut. A module doc
  that needs more is usually carrying what belongs in `docs/`.

Form: proper sentences, no parenthetical asides, no size claims. Doc
comments describe behavior, never lineage.
