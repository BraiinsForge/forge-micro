# edge-http: Add numeric Content-Length helper for initiate_response

## Problem

`Connection::initiate_response` accepts headers as `&[(&str, &str)]`. There is no
helper for supplying a numeric `Content-Length` value, so every downstream caller that
knows the body size up front must format the number into a stack-allocated byte buffer
by hand before building the header slice:

```rust
// Repeated in every handler across this workspace:
let mut length_buffer = [0u8; 20];
// ... manual decimal formatting ...
let content_length_str = /* &str into length_buffer */;
connection.initiate_response(200, Some("OK"), &[
    ("Content-Length", content_length_str),
    // ...
]).await?;
```

`Headers::set_content_len` exists but takes `&mut heapless::String<20>` as a buffer and
operates on the `Headers` struct, not on the `&[(&str, &str)]` slice expected by
`initiate_response`. There is no bridging helper.

## Reproduction

Upstream source: `edge-http/src/io/server.rs`, `Connection::initiate_response`,
and `edge-http/src/lib.rs`, `Headers::set_content_len`, commit
`1f084b41944c35b233cbe17accfbc09a56d5eaf0`.

`initiate_response` signature:

```rust
pub async fn initiate_response(
    &mut self,
    status: u16,
    message: Option<&str>,
    headers: &[(&str, &str)],
) -> Result<(), Error<T::Error>>
```

`Headers::set_content_len` exists at lib.rs line 356 but writes into a `heapless::String`
and is only useful when building a full `Headers` struct, not a header slice.

## Proposed fix

Two options are presented so the maintainer can choose the approach that fits the
library's design.

### Option A — free function `format_content_length`

Add a free function to the crate root that formats a `usize` into a caller-supplied
byte buffer and returns a `&str` slice, making the buffer lifetime explicit and
keeping `initiate_response` unchanged:

```diff
--- a/edge-http/src/lib.rs
+++ b/edge-http/src/lib.rs
@@ -14,6 +14,24 @@
 pub const DEFAULT_MAX_HEADERS_COUNT: usize = 64;
 
+/// Format `content_length` as ASCII decimal into `buf` and return a `&str`
+/// slice valid for the lifetime of `buf`.
+///
+/// `N` must be at least 20 to hold any `usize` on a 64-bit target.
+/// Pass the returned `&str` as the `"Content-Length"` header value to
+/// [`Connection::initiate_response`].
+///
+/// # Example
+/// ```rust
+/// let mut buf = [0u8; 20];
+/// let len_str = edge_http::format_content_length(body.len(), &mut buf);
+/// connection.initiate_response(200, Some("OK"), &[
+///     ("Content-Length", len_str),
+/// ]).await?;
+/// ```
+pub fn format_content_length<const N: usize>(value: usize, buf: &mut [u8; N]) -> &str {
+    let mut remaining = value;
+    let mut cursor = N;
+    loop {
+        cursor -= 1;
+        buf[cursor] = b'0' + (remaining % 10) as u8;
+        remaining /= 10;
+        if remaining == 0 { break; }
+    }
+    buf[cursor..].rotate_left(0); // no-op; digits already in place after loop
+    // Reverse so the most-significant digit is first.
+    buf[cursor..].reverse();
+    let written = N - cursor;
+    core::str::from_utf8(&buf[..written]).unwrap_or("0")
+}
```

Note: the exact implementation is illustrative; the maintainer may prefer a different
digit-reversal approach.

### Option B — `Connection::initiate_response_with_body`

Add a convenience method that takes the body as a `&[u8]` slice, infers
`Content-Length` automatically, sends the headers, writes the body, and finishes the
response:

```diff
--- a/edge-http/src/io/server.rs
+++ b/edge-http/src/io/server.rs
@@ -100,6 +100,31 @@
         self.complete_request(status, message, headers).await
     }
 
+    /// Initiate a response with a known body, adding `Content-Length` automatically.
+    ///
+    /// Equivalent to calling `initiate_response` with a `Content-Length` header
+    /// followed by `write_all(body)`. The caller does not need to manage a
+    /// length-formatting buffer.
+    pub async fn initiate_response_with_body(
+        &mut self,
+        status: u16,
+        message: Option<&str>,
+        headers: &[(&str, &str)],
+        body: &[u8],
+    ) -> Result<(), Error<T::Error>> {
+        let mut len_buf: heapless::String<20> = heapless::String::new();
+        use core::fmt::Write as _;
+        // heapless::String<20> can always represent any usize on supported targets.
+        let _ = core::write!(len_buf, "{}", body.len());
+        let mut full_headers: heapless::Vec<(&str, &str), { DEFAULT_MAX_HEADERS_COUNT }> =
+            heapless::Vec::new();
+        for &h in headers {
+            let _ = full_headers.push(h);
+        }
+        let _ = full_headers.push(("Content-Length", len_buf.as_str()));
+        self.complete_request(status, message, &full_headers).await?;
+        embedded_io_async::Write::write_all(self, body).await?;
+        Ok(())
+    }
```

Note: Option B requires a const-generic or fixed upper bound on the header count;
the exact signature may need adjustment to fit the crate's design.

### Recommendation

Option A is simpler: it adds a single `no_std`-safe free function with no new generics,
and leaves the caller in full control of the buffer and header assembly. Option B is more
ergonomic for the common case of small static responses but has more complex generics.

## Risk

Both options are additive; no existing API is modified. Option A has zero risk. Option B
introduces a new method with a const-generic bound that may conflict with future API
evolution of `Connection`.

## Suggested PR title

`edge-http: Add Content-Length formatting helper for initiate_response`

## Suggested PR body

Every `edge-http` server handler that sends a sized response must format the body
length into a temporary buffer before passing it to `initiate_response`. There is no
crate-provided helper for this.

Two options are proposed (details in the diff):

- **Option A** (`format_content_length` free function): minimal, `no_std`-safe, mirrors
  `Headers::set_content_len` but works with the `&[(&str,&str)]` header slice.
- **Option B** (`initiate_response_with_body`): higher-level convenience method that
  accepts a `&[u8]` body and infers `Content-Length` automatically.

Both are additive and non-breaking. The maintainer's preference on API style determines
which to land.
