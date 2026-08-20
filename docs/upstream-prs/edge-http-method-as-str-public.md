# edge-http: Make Method::as_str() public

## Problem

`edge_http::Method::as_str()` exists and is used internally by the `Display` impl and by
`send_request()` in `edge_http::io`, but its visibility is `fn` (private to the module).
Downstream crates that need the canonical uppercase method string — for routing, logging,
or forwarding — cannot call it directly. The only escape hatch is to go through the
`Display` impl and format into an intermediate heap- or stack-allocation, which is wasteful
and awkward in `no_std` contexts where all allocations are bounded and manual.

In this workspace one function replaces the missing public accessor.
`extract_request_parts` in `ubos/ubos-http/src/request.rs` formats the method through
the `Display` impl into a bounded buffer:

```rust
// `edge_http::Method::as_str` is private upstream, so the canonical
// method string is reached through the `Display` impl instead. See
// `docs/upstream-prs/edge-http-method-as-str-public.md`.
let mut method = String::<16>::new();
let _ = core::write!(method, "{}", headers.method);
```

Every caller that needs the method string reads it from the `RequestParts` that
function returns, so the workaround exists once.

## Reproduction

Upstream source: `edge-http/src/lib.rs`, `impl Method`, commit
`1f084b41944c35b233cbe17accfbc09a56d5eaf0`.

```rust
// line 177 — private
fn as_str(&self) -> &'static str {
    match self {
        Self::Delete => "DELETE",
        // ...
    }
}
```

The method is already fully implemented, returns `&'static str`, and is used in two
places internally (`Display` impl and `send_request`). The only change needed is
adding `pub`.

## Proposed fix

```diff
--- a/edge-http/src/lib.rs
+++ b/edge-http/src/lib.rs
@@ -174,7 +174,7 @@
     }
 
-    fn as_str(&self) -> &'static str {
+    pub fn as_str(&self) -> &'static str {
         match self {
             Self::Delete => "DELETE",
```

## Risk

Additive-only API change; making a private method public is not a breaking change.
No behaviour change for existing callers.

## Suggested PR title

`edge-http: Make Method::as_str() pub`

## Suggested PR body

`Method::as_str()` already exists and is used by the `Display` impl and
`send_request()`, but it is private. Downstream crates in `no_std` environments that
need the canonical method string for routing or logging are forced to go through
`Display` + a temporary `heapless::String<16>` allocation.

Adding `pub` to the existing method eliminates that workaround with zero risk — the
function body, return type, and semantics are unchanged.
