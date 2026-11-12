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

"""
Write the bytes of a symbol in a firmware ELF to standard output.

A firmware exports a value that a host tool needs under a symbol name,
such as the config schema hash that a packer writes into an update
header. The tool reads the value with this script from the ELF that the
image came from, so it gets the value that the image was built with.
"""

import argparse
import sys

from elftools.common.exceptions import ELFError
from elftools.elf.elffile import ELFFile


def symbol_bytes(path: str, name: str) -> bytes:
    """The bytes of the symbol `name` in the ELF at `path`.

    Exits with an error unless the ELF holds exactly one such symbol, with
    at least one byte, in a section that holds data, because the caller
    would otherwise use a value that the image was not built with.
    """
    with open(path, "rb") as stream:
        try:
            elf = ELFFile(stream)
        except ELFError:
            sys.exit(f"Error: {path} is not an ELF file. Give the ELF that the image came from.")
        symtab = elf.get_section_by_name(".symtab")
        if symtab is None:
            sys.exit(f"Error: {path} has no symbol table. Use an ELF that is not stripped.")
        symbols = symtab.get_symbol_by_name(name) or []
        if len(symbols) != 1:
            sys.exit(f"Error: {path} has {len(symbols)} {name} symbols. It must have exactly one.")
        symbol = symbols[0]
        size = symbol["st_size"]
        if size == 0:
            sys.exit(f"Error: The {name} symbol in {path} is empty. It must have at least one byte.")
        index = symbol["st_shndx"]
        section = elf.get_section(index) if isinstance(index, int) else None
        if section is None or section["sh_type"] != "SHT_PROGBITS":
            sys.exit(f"Error: The {name} symbol in {path} is not in a section that holds data.")
        start = symbol["st_value"] - section["sh_addr"]
        return section.data()[start:start + size]


def main():
    parser = argparse.ArgumentParser(description="Write the bytes of a symbol in a firmware ELF to standard output")
    parser.add_argument("elf", help="Firmware ELF that exports the symbol")
    parser.add_argument("symbol", help="Name of the symbol")
    args = parser.parse_args()
    sys.stdout.buffer.write(symbol_bytes(args.elf, args.symbol))


if __name__ == "__main__":
    main()
