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
Print the config schema hash that a firmware ELF declares, as hex.

ubos-config exports the schema hash of each build under one symbol. A
packer that writes the hash into an update header reads it with this
script from the ELF the image came from, so the header carries the hash
the image was built with.
"""

import argparse
import sys

from elftools.common.exceptions import ELFError
from elftools.elf.elffile import ELFFile

# The export name of SCHEMA_HASH_SYMBOL in ubos-config's versions module.
SYMBOL = "UBOS_CONFIG_SCHEMA_HASH"
# The length of a config schema hash in bytes.
HASH_LEN = 8


def schema_hash(path: str) -> bytes:
    """The bytes of the schema hash symbol in the ELF at `path`.

    Exits with an error unless the ELF holds exactly one such symbol, of
    the hash length, in a section that holds data, because a packer would
    otherwise write a hash the image was not built with.
    """
    with open(path, "rb") as stream:
        try:
            elf = ELFFile(stream)
        except ELFError:
            sys.exit(f"Error: {path} is not an ELF file. Give the ELF that the image came from.")
        symtab = elf.get_section_by_name(".symtab")
        if symtab is None:
            sys.exit(f"Error: {path} has no symbol table. Use an ELF that is not stripped.")
        symbols = symtab.get_symbol_by_name(SYMBOL) or []
        if len(symbols) != 1:
            sys.exit(f"Error: {path} has {len(symbols)} {SYMBOL} symbols. It must have exactly one.")
        symbol = symbols[0]
        if symbol["st_size"] != HASH_LEN:
            sys.exit(f"Error: The {SYMBOL} symbol in {path} has {symbol['st_size']} bytes. It must have {HASH_LEN} bytes.")
        index = symbol["st_shndx"]
        section = elf.get_section(index) if isinstance(index, int) else None
        if section is None or section["sh_type"] != "SHT_PROGBITS":
            sys.exit(f"Error: The {SYMBOL} symbol in {path} is not in a section that holds data.")
        start = symbol["st_value"] - section["sh_addr"]
        return section.data()[start:start + HASH_LEN]


def main():
    parser = argparse.ArgumentParser(description="Print the config schema hash that a firmware ELF declares")
    parser.add_argument("elf", help="Firmware ELF that links ubos-config")
    args = parser.parse_args()
    print(schema_hash(args.elf).hex())


if __name__ == "__main__":
    main()
