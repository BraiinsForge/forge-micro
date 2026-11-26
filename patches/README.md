# Upstream patch series

The repo carries only patch files against pinned upstream sources,
never a vendored crate tree; nix materializes each patched tree at
build time (see `nix/patched-crates.nix` and `nix/probe-rs.nix`).
A patch is a derivative of the file it modifies, so each series below
carries the license of its upstream project, not the repository-wide
GPL-3.0-or-later grant.

| Series | Upstream project | Pin | License |
|--------|------------------|-----|---------|
| `edge-http-*.patch` | [ivmarkov/edge-net](https://github.com/ivmarkov/edge-net), `edge-http` subdirectory | git `1f084b41944c35b233cbe17accfbc09a56d5eaf0` | MIT OR Apache-2.0 |
| `edge-nal-embassy-*.patch` | [ivmarkov/edge-net](https://github.com/ivmarkov/edge-net), `edge-nal-embassy` subdirectory | git `1f084b41944c35b233cbe17accfbc09a56d5eaf0` | MIT OR Apache-2.0 |
| `embassy-net-wiznet-*.patch` | [embassy-rs/embassy](https://github.com/embassy-rs/embassy), `embassy-net-wiznet` crate | crates.io release 0.3.0 | MIT OR Apache-2.0 |
