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
# Braiins Systems s.r.o. reserves the right to grant any party a license
# to this program, or any part thereof, under any terms, and such a grant
# shall be considered distinct from the grant above.

# Writes the bytes of a symbol in a firmware ELF to standard output, for a
# host tool that needs a value the image was built with, such as a packer
# that writes the config schema hash into an update header. Packaged with
# writePython3Bin so pyflakes and pyelftools come for free.
{
  writers,
  python3Packages,
}:
writers.writePython3Bin "elf-symbol" {
  libraries = [ python3Packages.pyelftools ];
  # E501: each error message stays on one line rather than wrapping at 79
  # columns.
  flakeIgnore = [ "E501" ];
} (builtins.readFile ./elf_symbol.py)
