---
paths:
  - "**/Cargo.toml"
---

# New crates

**A new crate is never created without the user's explicit approval.**
Ask, and wait for the answer. This holds however small the crate looks,
and whatever else the user has already approved: approval of a plan that
happens to mention a crate is not approval of the crate. The question is
asked in its own right, with the alternatives below named beside it.

A crate is not a file. It is a manifest, a row in the workspace
`members` list, an entry in `[workspace.dependencies]`, a `clippy-*` and
usually a `test-*` entry in `nix/checks.nix`, a row in this tree's
`docs/STRUCTURE.md`, which is the one crate ledger for both trees of
the pair, since the repo that symlinks this rule keeps no ledger of its
own, and a name every later reader has to learn before they can tell
where code belongs. That cost is paid on every future read, and the
crate outlives the reason it was added. The reason is usually a handful
of lines that needed a home for an afternoon.

Reach for these first, in this order, and say which you rejected and
why when you do ask:

1. **Put it where it is used.** A helper with one consumer is a private
   module of that consumer, or a `#[cfg(test)]` module of it.
2. **Delete the need.** Test scaffolding especially: a simpler shape
   often removes the shared thing rather than rehousing it. A waker that
   tracks wakes becomes a fixed count of polls; a trait object becomes a
   concrete type.
3. **Copy it.** Two copies of thirty lines in two test modules cost less
   than a crate, and the second copy is the signal that tells you when a
   home is finally worth it. Duplication is cheaper than the wrong
   abstraction.
4. **Widen what is already there.** A `pub(crate)` that becomes `pub`, or
   a constructor beside an existing accessor, usually reaches further
   than a new crate would.

The case that does justify one is a body of code with more than one
genuine consumer, a name that will still be right in a year, and no
existing crate whose charter covers it. Two consumers that are both
test modules of the same subsystem are one consumer wearing two hats.

The same question, asked the same way, covers a new workspace member of
any kind, and a new top-level directory.
