# Small helpers shared by more than one flake module: the
# `CC_<target>` env-var name cc-rs expects, the ARM cross-CC attrset
# every ARM crane derivation carries, the xtensa cross-build env every
# esp32s3 crane derivation carries, and the ubos-webui build-env attrs
# that every derivation and dev shell touching the dashboard must set
# so build.rs stamps a consistent rev and time. Defined once here so
# the copies in checks.nix, images.nix, and shells.nix cannot drift
# apart.
{
  pkgs,
  webuiBuildEnv,
  espToolchain,
  xtensaEspToolchain,
}:
rec {
  ccVar = target: "CC_${builtins.replaceStrings [ "-" "." ] [ "_" "_" ] target}";

  armCc = target: {
    "${ccVar target}" = "${pkgs.gcc-arm-embedded}/bin/arm-none-eabi-gcc";
  };

  webuiEnvAttrs = {
    inherit (webuiBuildEnv) UBOS_WEBUI_BUILD_REV UBOS_WEBUI_BUILD_TIME;
  };

  # Xtensa cross-build env every esp32s3 crane derivation carries:
  # rust-src for -Z build-std, the espressif cross-gcc for the
  # secp256k1-sys C build script, and -mlongcalls so its memset/memcpy
  # can be reached from text sections beyond the xtensa call8 range.
  espCrossEnv = {
    RUST_SRC_PATH = "${espToolchain}/lib/rustlib/src/rust/library";
    CC_xtensa_esp32s3_none_elf = "${xtensaEspToolchain}/bin/xtensa-esp32s3-elf-gcc";
    CFLAGS_xtensa_esp32s3_none_elf = "-mlongcalls";
  };

  # Linker rustflags for the esp32s3 target. Crane runs cargo from the
  # workspace root, so firmware/esp32s3/.cargo/config.toml is out of
  # cargo's discovery path; without -Tlinkall.x the linker uses default
  # section placement and secp256k1-sys's memset/memcpy calls overflow
  # the xtensa windowed-call range.
  espRustflags = {
    CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS = "-Clink-arg=-Tlinkall.x -Clink-arg=-Tdefmt.x -Clink-arg=-nostartfiles";
  };
}
