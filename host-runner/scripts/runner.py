#!/usr/bin/env python3

# Copyright (C) 2019  Braiins Systems s.r.o.
#
# This file is part of Braiins Open-Source Initiative (BOSI).
#
# BOSI is free software: you can redistribute it and/or modify
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
# Please, keep in mind that we may also license BOSI or any part thereof
# under a proprietary license. For more information on the terms and conditions
# of such proprietary license or if you have any other questions, please
# contact us at opensource@braiins.com.

"""
Cargo runner for cross-compiled host-runner binaries.

Copies the binary to a remote miniminer (or compatible OpenWrt target) via
scp, runs it under a PTY, forwards the exit code, and cleans up. Always
stops any running bosminer first to free the hash boards. OpenWrt's `lock`
serializes concurrent runner invocations.

Vendored and trimmed from bos-main's bosminer/scripts/runner.py.

Configuration: pass --hostname (and optionally --user/--port/--path) on the
command line, or place a Test.toml beside (or one level above) the cwd:

    [remote]
    user     = "root"
    hostname = "10.0.0.1"
    port     = 22
"""

import sys
import os.path
import argparse
import tomllib
import subprocess
import shlex
from time import sleep
from datetime import datetime

CONFIG_FILE = 'Test.toml'
DEFAULT_USER = 'root'
CONFIG_PATHS = ['.', '..']
PROPAGATE_ENV_VARS = ['RUST_BACKTRACE', 'RUST_LOG']


def main():
    parser = argparse.ArgumentParser(__doc__)

    parser.add_argument('test', help='Path to executable.')
    arg_hostname = parser.add_argument(
        '--hostname',
        help='IP address or hostname of remote target with sshd.',
    )
    parser.add_argument(
        '--port', help='Port if ssh is not running on port 22.'
    )
    parser.add_argument('--keep', action='store_true', help='Keep remote file.')
    parser.add_argument(
        '--path',
        dest='host_path',
        default='/tmp',
        metavar='PATH',
        help='Target path on remote host.',
    )
    parser.add_argument('--user', help='Remote login user.')
    parser.add_argument('--no-lock', action='store_true', help='Do not lock target.')
    parser.add_argument(
        '--verbose', action='store_true', help='Log commands to stdout.'
    )
    parser.add_argument('--log', action='store_true', help='Create session logs.')
    parser.add_argument(
        '--password',
        type=str,
        help='Password for the host. Not used when empty.',
    )

    # parse command line arguments
    (args, extra_args) = parser.parse_known_args()

    runner = run_verbose if args.verbose else run_silent

    do_locking = not args.no_lock
    do_keep = args.keep
    do_log = args.log

    cfg_user = None
    cfg_hostname = None
    cfg_port = None

    # construct all config file locations
    cfg_locations = [os.path.join(dir, CONFIG_FILE) for dir in CONFIG_PATHS]
    cfg_path = next((path for path in cfg_locations if os.path.isfile(path)), None)

    if cfg_path is not None:
        # try to get default configuration from configuration file
        with open(cfg_path, 'rb') as f:
            config = tomllib.load(f)
        remote = config.get('remote')
        if remote:
            cfg_user = remote.get('user')
            cfg_hostname = remote.get('hostname')
            cfg_port = remote.get('port')

    # get remote settings
    user = args.user or cfg_user or DEFAULT_USER
    hostname = args.hostname or cfg_hostname
    port = args.port or cfg_port

    if not hostname:
        parser.error(
            "Missing remote hostname which can be specified by '{}' argument or in '{}'".format(
                arg_hostname.option_strings[0], CONFIG_FILE
            )
        )

    test_path = args.test
    test_name = os.path.basename(args.test)
    remote_argv = list(extra_args)
    remote_test = os.path.join(args.host_path, test_name)

    # build a runner
    host_runner = Runner(
        user, hostname, port=port, verbose=args.verbose, password=args.password
    )
    host_runner.extend_args(['-o', 'StrictHostKeyChecking=no'])

    # Always stop any running bosminer; the hash boards cannot be shared.
    host_runner.execute(
        '/etc/init.d/bosminer stop; /etc/init.d/S99bosminer stop; killall bosminer-plus-tuner; killall bosminer'
    )

    # Wait for bosminer to actually exit.
    while True:
        ps_output = host_runner.execute(
            'ps aux || ps', capture_output=True, check=True
        )
        processes = ps_output.stdout.splitlines()

        running_bosminer = any(
            'bosminer' in str(line) and not 'inotify' in str(line)
            for line in processes
        )
        if not running_bosminer:
            print('No bosminer is running, continuing')
            break
        sleep(1)

    if do_locking:
        # verify if a stale lock is present on the miner
        stale_locks = host_runner.execute(
            'test -e /tmp/testrunner && ps', capture_output=True
        )

        processes = stale_locks.stdout.splitlines()
        # format: pid, user, vsz, stat, command...
        # skip empty entries
        processes = map(
            lambda process_line: [word for word in process_line.split() if word],
            processes,
        )

        active_process_exists = any(
            (
                'lock' in line
                or '/tmp/testrunner' in line
                or 'scp' in line
            )
            # S indicates a suspended process
            and line[3] != 'S'
            for line in processes
        )

        if stale_locks.returncode == 0 and not active_process_exists:
            print('deleting probably stale lock')
            host_runner.execute('rm /tmp/testrunner', check=True)
            host_runner.execute('killall lock')

        # create target dir if not exist and acquire a lock, in case multiple runners meet.
        # note this locking is openwrt specific, and unlike flock have to be explicitely unlocked
        host_runner.execute(
            'mkdir -p ' + args.host_path + ' && lock /tmp/testrunner', check=True
        )

    md5sum_of_binary = (
        runner('md5sum', test_path, check=True, capture_output=True)
        .stdout.strip()
        .split()
    )
    md5sum_of_remote = (
        host_runner.execute('md5sum {}'.format(remote_test), capture_output=True)
        .stdout.strip()
        .split()
    )

    try:
        if (
            len(md5sum_of_binary) > 0
            and len(md5sum_of_remote) > 0
            and md5sum_of_binary[0] == md5sum_of_remote[0]
        ):
            print('binary with this hash is already on the remote, skipping copying')
        else:
            if args.verbose:
                print('copying %d bytes' % os.path.getsize(test_path))

            host_runner.scp(
                test_path,
                args.host_path,
                check=True,
            )

        remote_test_cmd = remote_test
        for env_var in PROPAGATE_ENV_VARS:
            if env_var in os.environ:
                remote_test_cmd = '{}={} {}'.format(
                    env_var, os.environ[env_var], remote_test_cmd
                )
        for key, value in os.environ.items():
            if key.startswith('INJECT_'):
                remote_test_cmd = '{}={} {}'.format(key, value, remote_test_cmd)

        log_name = (
            'runner_log_'
            + hostname
            + '_'
            + datetime.now().strftime('%Y-%m-%d_%H:%M:%S')
            + '.log'
        )
        run_ret = host_runner.execute(
            remote_test_cmd + ' ' + ' '.join(remote_argv),
            pty=True,
            check=False,
            log_to=log_name if do_log else None,
        )

    finally:
        if do_locking:
            # clean up code and lock
            host_runner.execute(
                ('rm -f ' + remote_test + ' ; ' if not do_keep else '')
                + 'lock -u /tmp/testrunner',
                check=True,
            )

    sys.exit(run_ret.returncode)


def run_silent(*args, **kargs):
    return subprocess.run(args, **kargs)


def run_verbose(*args, **kargs):
    """wrap subprocess.run to log executed commands on stdout"""
    print('Executing: ' + ' '.join(args), flush=True)
    return run_silent(*args, **kargs)


class Runner:
    def __init__(self, user=None, host=None, port=None, verbose=False, password=None):
        self.user = user
        self.host = host
        self.verbose = verbose
        self.port = port
        self.password = password
        self.extra_args = []

    def extend_args(self, args):
        self.extra_args.extend(args)

    def scp(self, from_path, to_path, **kargs):
        # no progressbar, enable compression
        command = ['scp', '-q', '-C', '-O']

        if self.port is not None:
            command.extend(['-P', str(self.port)])

        command.extend(self.extra_args)
        command.append(from_path)
        command.append(f'{self.user}@{self.host}:{to_path}')

        if self.password:
            command = ['sshpass', '-p', self.password] + command

        if self.verbose:
            print(f'Executing: {" ".join(command)}', flush=True)
        result = subprocess.run(command, **kargs)

        return result

    def execute(self, cmd, pty=False, log_to=None, **kargs):
        command = ['ssh']

        if not self.verbose:
            command.append('-q')

        if self.port is not None:
            command.extend(['-p', str(self.port)])

        if pty:
            command.append('-t')

        command.extend(self.extra_args)

        command.append(f'{self.user}@{self.host}')
        command.append(cmd)

        if self.password:
            command = ['sshpass', '-p', self.password] + command

        if self.verbose:
            print(f'Executing: {" ".join(command)}', flush=True)
        if log_to is not None:
            print(f'Logging to file {log_to}', flush=True)
            # After an hour of trying to convince subprocess.Popen to pipe between two processes
            # and correctly handle stdout/stderr and return codes, I gave up and wrote this shit.
            result = subprocess.run(
                shlex.join(command) + f'| tee -a {log_to}', shell=True, **kargs
            )
        else:
            result = subprocess.run(command, **kargs)

        return result


if __name__ == '__main__':
    # execute only if run as a script
    main()
