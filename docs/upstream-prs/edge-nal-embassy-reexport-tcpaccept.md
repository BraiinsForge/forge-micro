# edge-nal-embassy: Re-export TcpAccept, TcpBind, TcpConnect traits from edge-nal

## Problem

`edge_nal_embassy` provides `Tcp`, `TcpAccept` (struct), `TcpBuffers`, and related types
through its `pub use tcp::*` re-export. However, the *traits* `edge_nal::TcpAccept`,
`edge_nal::TcpBind`, and `edge_nal::TcpConnect` live in `edge_nal` and are not
re-exported by `edge_nal_embassy`.

This means that every crate using `edge_nal_embassy` must also explicitly depend on
`edge_nal` and import the traits by name to call any trait methods. In practice, calling
`.bind()` on `Tcp` or `.accept()` on the acceptor requires:

```rust
use edge_nal::{TcpAccept as _, TcpBind as _};
use edge_nal_embassy::{Tcp, TcpBuffers};
```

The `use edge_nal` line is pure boilerplate; the user is working entirely with
`edge_nal_embassy` types and should not need to reach into the lower-level crate just to
unlock method dispatch.

## Reproduction

Upstream source: `edge-nal-embassy/src/lib.rs`, commit
`1f084b41944c35b233cbe17accfbc09a56d5eaf0`.

The file contains:

```rust
#[cfg(feature = "tcp")]
pub use tcp::*;
```

`tcp.rs` re-exports `Tcp`, the struct `TcpAccept`, `TcpSocket`, `TcpError`, etc., but
none of the `edge_nal` traits. A downstream crate cannot call `tcp.bind(addr).await`
without a separate `use edge_nal::TcpBind as _` import.

Evidence from this workspace (`ubos-metrics/src/metrics.rs` line 9):

```rust
use edge_nal::{TcpAccept as _, TcpBind as _};
use edge_nal_embassy::{Tcp, TcpBuffers};
```

## Proposed fix

```diff
--- a/edge-nal-embassy/src/lib.rs
+++ b/edge-nal-embassy/src/lib.rs
@@ -13,6 +13,11 @@
 #[cfg(feature = "dns")]
 pub use dns::*;
+// Re-export the edge-nal traits so callers do not need a separate
+// `edge_nal` dependency just to call .connect(), .bind(), or .accept().
+#[cfg(feature = "tcp")]
+pub use edge_nal::{TcpAccept, TcpBind, TcpConnect};
 #[cfg(feature = "tcp")]
 pub use tcp::*;
 #[cfg(feature = "udp")]
 pub use udp::*;
```

The trait re-exports are gated on the same `tcp` feature flag as the type re-exports
so no dependency is pulled in when `tcp` is disabled.

`TcpSplit`, `TcpShutdown`, `Readable` are already implicitly available through the
`TcpSocket` type constraints but would also be worth including for completeness; the
maintainer may prefer to add all relevant `edge_nal` traits at once. At minimum the
three listed (`TcpAccept`, `TcpBind`, `TcpConnect`) are the ones required to dispatch
on any `edge_nal_embassy` acceptor or connector.

## Risk

Additive re-export only. No behaviour change. In the unlikely event that a downstream
crate defines its own `TcpAccept`/`TcpBind`/`TcpConnect` name in the same scope as
`use edge_nal_embassy::*`, the new re-export would create a name conflict — but that is
an expected consequence of glob imports and not a semver break.

## Suggested PR title

`edge-nal-embassy: Re-export TcpAccept, TcpBind, TcpConnect traits`

## Suggested PR body

Using `edge_nal_embassy` types currently requires a separate `use edge_nal::{TcpBind as
_, TcpAccept as _}` import even though none of the code touches `edge_nal` types
directly. The calling crate must also add an explicit `edge_nal` dependency.

Adding `pub use edge_nal::{TcpAccept, TcpBind, TcpConnect}` (feature-gated on `tcp`,
matching the existing `pub use tcp::*`) makes the common case work with a single
`use edge_nal_embassy::*` or targeted import, without requiring knowledge of the
internal crate split.

Pure additive change, no API break.
