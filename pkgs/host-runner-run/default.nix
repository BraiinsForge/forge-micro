# Copyright (C) 2026  Braiins Systems s.r.o.
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.
#
# Cargo runner for cross-compiled host-runner binaries: copies the binary
# to a remote target via scp and runs it under ssh. Packaged with
# writePython3Bin so pyflakes and its library deps come for free. It shells
# out to ssh/scp/sshpass/md5sum, which are not runtimeInputs of a Python
# writer; the consuming shell must put them on PATH.
{
  writers,
}:
writers.writePython3Bin "host-runner-run" {
  # E501: the script keeps one option/log line per statement rather than
  # wrapping at 79 columns. E713: pre-existing `not X in Y` membership
  # check, left as is rather than rewritten during the move. W503: passing
  # --ignore to flake8 replaces its default ignore list rather than
  # extending it, so W503 (line break before a binary operator, disabled
  # by default because it contradicts PEP 8's current guidance) has to be
  # restated here or it resurfaces on every multi-line boolean expression.
  flakeIgnore = [
    "E501"
    "E713"
    "W503"
  ];
} (builtins.readFile ./runner.py)
