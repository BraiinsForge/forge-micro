---
paths:
  - "**/*.nix"
  - "**/*.rs"
---

# Named constants

A literal that carries meaning gets a name, and the name is what the code
uses. Bind it once in the scope that owns it, then refer to it
everywhere — including inside generated shell, error messages, and log
lines, which is where the second copy usually creeps in.

- **Nix**: a `let` binding, interpolated as `${toString name}`.
  `slotMaxKiB` and `claudeMdMaxLines` in `nix/checks.nix` are the
  pattern.
- **Rust**: a `const`, or a `static` where it must have an address,
  scoped to the module that owns it.

The failure this prevents is one literal written two or three times — the
comparison, the message that reports it, and a comment restating it —
where the next edit moves one copy and leaves the others behind. A second
copy of a number is a bug that has not fired yet.

Where a limit is genuinely shared across languages that cannot import
each other, name it on both sides and gate the agreement; the
`ota-image-limit` check is how that is done here.

Exempt are the values with no other name: `0`, `1`, and the indices and
offsets whose meaning is exhausted by the expression they sit in.

## Across a crate boundary

A value a crate tunes for its own behavior, such as a retry delay, a
hold time or a settle window, is internal to that crate. It is never a
`pub const`, and no other crate reads it, compares against it, or
asserts on it. What another crate needs reaches it one of two ways:

- **Runtime facts** go through the owning crate's `Handle`, as the
  state the value produces rather than the value. The cooling loop
  reads whether the rail is off from `ubos_psu::Handle::subscribe_rail`,
  not how long the miner holds it off.
- **Static board facts** go in the board's descriptor, which every
  consumer reads from the same place.

A consumer that seems to need another crate's timing is coupled the
wrong way. Make it decide on its own state instead. A `pub const`
remains correct for a value both sides use by definition, such as a
capacity in a public type or a field width in a wire format.
