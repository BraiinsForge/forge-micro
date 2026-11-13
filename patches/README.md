# Upstream patch series

The repo carries only patch files against pinned upstream sources,
never a vendored crate tree; nix materializes each patched tree at
build time (see `nix/patched-crates.nix` and `nix/probe-rs.nix`).
A patch is a derivative of the file it modifies, so each series below
carries the license of its upstream project, not the repository-wide
GPL-3.0-or-later grant.

| Series | Upstream project | Pin | License |
|--------|------------------|-----|---------|
| `edge-nal-embassy-*.patch` | [ivmarkov/edge-net](https://github.com/ivmarkov/edge-net), `edge-nal-embassy` subdirectory | git `6f7e384ff738aef38f3c38a38225a6ed1cfbac20` | MIT OR Apache-2.0 |
