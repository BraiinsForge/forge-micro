---
paths:
  - "**/*.rs"
  - "**/*.nix"
  - "**/*.c"
  - "**/*.py"
  - "**/*.js"
  - "**/*.sh"
  - "**/*.css"
  - "**/*.html"
---

# Licence headers

This repo is public and GPL-3.0-or-later throughout, so a new first-party
source in one of the eight extensions above carries the block. Copy it
from a neighbouring file in the same language and keep that language's
comment leader; it goes at the top of the file, above the module
documentation. The copyright line is a single year, the header is prose,
and there is no SPDX identifier.

The block has two parts, and both are required. The first is the
GPL-3.0-or-later grant. The second reserves the right of Braiins Systems
s.r.o. to grant a licence under other terms, and it is the part that is
easy to lose by copying only as far as the `licenses/` URL. The
proprietary crates in the internal repo link these crates into a shipped
product on that reservation and not on the repository layout, so a file
carrying the grant alone weakens the claim for that file.

Two kinds of file keep a licence that is not ours to touch. Vendored
`patches/*` trees keep upstream's. `assets/69-probe-rs.rules` and
`ubos/ubos-webui/web/icons.svg` are bundled material under another
licence, and each keeps its own licence text next to it:
`assets/LICENSE-PROBE-RS` for the udev rules file and
`ubos/ubos-webui/web/LICENSE-CARBON` for the icon set.

The manifests are the other half of the statement. Every crate spells
`license.workspace = true` and the root `[workspace.package]` supplies
`GPL-3.0-or-later`, so a new member inherits the terms by saying nothing.
Every member also spells `publish.workspace = true`, and the root
supplies `publish = false`, because nothing here is released as a crate.
The `micro-sv2` crates are outside this workspace, so each one spells
`publish = false` itself. That mark is what lets `cargo-deny` hold the
third-party graph to permissive licences while it passes ours, so a
crate without it fails that check.

The rule is gated rather than trusted. `license-headers` in
`nix/checks.nix` reads the head of every file in the eight extensions
outside `patches/`, strips the comment leader, and fails unless both
phrases are there. The extension list in that check is the scope: a
source in a language not on it is ungated, so a new language costs an
edit there as well as a header. The bundled exceptions need no exemption
list, because none of them has a gated extension.

A source in the internal repo is the opposite case and carries no licence
header at all, gated there by a `license-headers` check that fails on any
open grant. Neither tree's rule rests on the next edit noticing.

A file of this tree that the internal repo symlinks into its own tree
stays this tree's file, and it carries the block. The link is a name and
not a copy, and the check there reads with `grep -r`, which does not
descend a symlink it meets in recursion, so the two gates never meet over
one file. `.claude/hooks/gate-invocation` is the one of them today.
Copying such a file instead of linking it is what would need an exemption
there, and it is the reason to link it.
