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
  scoped to the module that owns it. `pub const` when another crate
  compares against the same value.

The failure this prevents is one literal written two or three times — the
comparison, the message that reports it, and a comment restating it —
where the next edit moves one copy and leaves the others behind. A second
copy of a number is a bug that has not fired yet.

Where a limit is genuinely shared across languages that cannot import
each other, name it on both sides and gate the agreement; the
`ota-image-limit` check is how that is done here.

Exempt are the values with no other name: `0`, `1`, and the indices and
offsets whose meaning is exhausted by the expression they sit in.
