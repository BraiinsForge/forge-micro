# Shared shell guards for firmware images.
#
# Every shipped binary must fit its OTA slot budget and must not bake
# in a pool worker. POOL_USER overrides the empty default at build
# time; a developer uses POOL_USER=braiinstest to test mining. A pure
# nix build strips POOL_USER from the sandbox, so the default empty
# worker is always used and a worker cannot leak; the POOL_USER guard
# only bites on an `--impure` build that forwarded it, where it fails
# the build before such an artifact escapes. Rendering the guard from
# one function keeps the per-board copies from drifting apart.
{ }:
{
  # Render the slot-size and pool-worker guard for one binary.
  # `bin` is a shell word naming the file to inspect; quote it at the
  # call site if it is a variable reference. A null `maxBytes` skips
  # the size check for boards that are not OTA targets. `cleanup` is
  # prepended to each failing exit, e.g. to remove a temp file.
  imageGuard =
    {
      name,
      bin,
      maxBytes ? null,
      budgetLabel ? "slot limit",
      cleanup ? "",
    }:
    (
      if maxBytes == null then
        ""
      else
        ''
          size=$(wc -c < ${bin})
          echo "${name} image size: $size bytes / ${toString maxBytes} bytes (${budgetLabel})"
          if [ "$size" -gt "${toString maxBytes}" ]; then
            echo "ERROR: ${name} release image ($size bytes) exceeds the" \
                 "${toString maxBytes}-byte ${budgetLabel}. See docs/OTA.md."
            ${cleanup}exit 1
          fi
        ''
    )
    + ''
      if [ -n "''${POOL_USER:-}" ]; then
        echo "ERROR: POOL_USER is set; a shipped image must not bake a pool worker."
        ${cleanup}exit 1
      fi
      if grep -qa "braiinstest" ${bin}; then
        echo "ERROR: ${name} image contains the test pool worker 'braiinstest'; it must not ship."
        ${cleanup}exit 1
      fi
    '';
}
