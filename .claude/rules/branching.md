---
paths:
  - "**/*.nix"
  - "**/*.rs"
---

# Branching

A conditional is a cost the reader pays on every visit, and an `else`
branch doubles it: the reader must now hold two states and work out
which one they are in. Most of the conditionals that creep into this
code are not decisions at all. They are facts about a thing that were
written as a test on some other fact about it.

Prefer the shape that states the fact.

- **Data, not a test.** A value that differs by kind is stated on the
  kind. `nix/images.nix` builds the rp2350 and the rp2040 bootloader
  with two calls to `mkBootloaderElf`, each stating its own target,
  rather than one call asking `if chip == "rp2040" then
  "thumbv6m-none-eabi" else "thumbv8m.main-none-eabihf"`. An enum
  variant carries its own fields; a match on a tag to pick a value that
  belongs to that tag is a field written as a branch.
- **Absent, not null.** An attribute one kind lacks is left out, not set
  to `null` behind an `else`. A reader that is not gated on the kind
  then stops at evaluation instead of carrying a placeholder onward.
  `Option` is the same thing in Rust, and its `?` is the guard.
- **Guard and return early.** Where a test is real, handle the failing
  case first and leave. The rest of the function then reads at one
  level, with no `else` to close and no state to carry.
- **Defaults instead of `or`.** A parameter that usually takes one
  value takes it as a default, `arg ? value`, and a caller that differs
  states the difference. The same in Rust is a constructor for the
  common case, not a flag threaded through one.
- **No boolean parameter that selects behaviour.** A `bool` argument
  that picks between two bodies is two functions folded into one; write
  the two.

A branch that survives this is one where both arms are live at run
time and the test is on input, not on a fact the code already knows.
Those stay, and keep the failing arm short.
