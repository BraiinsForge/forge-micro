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

# The share of its own lines, in percent, that the tests of each coverage
# row have to reach; the `line-coverage-floors` check holds every row to
# it. A row is held to the target of its kind once it reaches it, and to
# its present coverage until then, so the number here only moves up, by
# the change that adds the tests.
let
  targets = {
    # A fault in these can damage a board or leave it unable to boot.
    board = 85;
    # Protocol and state logic, which runs whole on the host.
    logic = 80;
  };
  # Code around a socket, a file or a device, which a host test reaches
  # in small part. The CRAP gate alone holds these rows.
  glue = 0;
in
{
  inherit targets;

  floors = {
    ubos-asic = 81;
    ubos-cooling = 84;
    ubos-eeprom = 80;
    ubos-mining = 79;
    ubos-psu = 83;
    ubos-temperature = 26;
    ubos-thermal = targets.board;

    mining-protocol = targets.logic;
    stratum-client = 75;
    stratum-v1 = 79;
    stratum-v2 = 77;
    transitions = targets.logic;
    ubos-api = 31;
    ubos-auth = targets.logic;
    ubos-build = 14;
    ubos-config = 61;
    ubos-config-off = 60;
    ubos-config-std = 73;
    ubos-core = 79;
    ubos-events = targets.logic;
    ubos-manager = 38;
    ubos-snapshot = targets.logic;

    embedded-i2c-unix = glue;
    host-runner = glue;
    ubos-http = glue;
    ubos-machine = glue;
    ubos-mdns = glue;
    ubos-metrics = glue;
    ubos-net = glue;
    ubos-webui = glue;
  };
}
