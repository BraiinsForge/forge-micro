# Cargo runner shared by the probe-rs boards (bmm-pico, rp2040, rp2350):
# a writeShellApplication so shellcheck and the probe-rs-runner PATH dep
# come for free. Each crate's .cargo/config.toml sets PROBE_RS_CHIP in
# [env] and points its runner at this binary. Private to the internal
# tree, stripped from the mirror with the boards it serves.
{
  writeShellApplication,
  probe-rs-runner,
}:
writeShellApplication {
  name = "probe-rs-board-run";
  runtimeInputs = [ probe-rs-runner ];
  text = builtins.readFile ./run.sh;
}
