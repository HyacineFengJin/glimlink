#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Native synthetic X window acceptance. No profiles, accounts or X network.

Use isolate.sh client-test operator-webview + private D-Bus/Xvfb. The separately
feature-gated executable uses loopback HTML and Rust-only HttpOnly/Secure cookie
injection. A private fake xdg-open verifies the fixed URL without launching a
browser or navigating to X. All output contains acceptance booleans/accounting.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

spec = importlib.util.spec_from_file_location('desktop_accounting', Path(__file__).with_name('validate-operator-desktop.py'))
accounting = importlib.util.module_from_spec(spec)
spec.loader.exec_module(accounting)
parser = argparse.ArgumentParser()
parser.add_argument('--binary', default='/cache/operator-desktop-target/debug/x-login-fixture')
parser.add_argument('--report', type=Path, default=Path('/reports/operator-x-login-window-native.json'))
args = parser.parse_args()
peak_rss = peak_pss = 0
with tempfile.TemporaryDirectory(prefix='operator-x-native-') as temporary, \
     tempfile.TemporaryDirectory(prefix='operator-x-opener-', dir='/cache') as executable_directory:
    root = Path(temporary)
    # Docker's /tmp remains noexec. Only this synthetic executable belongs in
    # the private development cache; do not weaken the container mount policy.
    binaries = Path(executable_directory) / 'bin'
    binaries.mkdir(mode=0o700)
    opener = binaries / 'xdg-open'
    opener.write_text('''#!/bin/sh
set -eu
[ "$#" -eq 1 ] && [ "$1" = 'https://x.com/i/flow/login' ] || exit 1
printf 'fixed-x-url\\n' >> "$AGENT_OPERATOR_SYNTHETIC_OPENER_RESULT"
''')
    opener.chmod(0o700)
    (root / 'run').mkdir(mode=0o700)
    environment = dict(os.environ, HOME=temporary, XDG_CONFIG_HOME=str(root / 'config'), XDG_CACHE_HOME=str(root / 'cache'), XDG_DATA_HOME=str(root / 'data'), XDG_RUNTIME_DIR=str(root / 'run'), AGENT_RUNTIME_STATE=str(root / 'empty-state'), AGENT_OPERATOR_SYNTHETIC_OPENER_RESULT=str(root / 'opener-result'), PATH=str(binaries) + os.pathsep + os.environ['PATH'])
    with args.report.with_suffix('.stderr').open('wb') as errors:
        application = subprocess.Popen([args.binary], cwd=temporary, env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=errors, text=True, start_new_session=True)
        deadline = time.monotonic() + 35
        try:
            while application.poll() is None:
                assert time.monotonic() < deadline, 'native fixture timed out'
                table = accounting.processes()
                owned = accounting.owned_processes(application.pid, table)
                peak_rss = max(peak_rss, sum(table[pid][2] for pid in owned))
                peak_pss = max(peak_pss, sum(accounting.pss(pid) for pid in owned))
                time.sleep(.04)
            output = application.stdout.read(16384)
            assert application.returncode == 0, f'native fixture failed ({application.returncode}); see synthetic stderr report'
            report = json.loads(output.strip())
            assert report and report['real_account_used'] is False
            assert all(value is True for key, value in report.items() if key != 'real_account_used')
            cgroup = Path('/sys/fs/cgroup')
            memory_limit = int((cgroup / 'memory.max').read_text())
            memory_peak = int((cgroup / 'memory.peak').read_text())
            events = dict(line.split() for line in (cgroup / 'memory.events').read_text().splitlines())
            assert memory_limit == 1024**3 and memory_peak < memory_limit and int(events['oom_kill']) == 0
            report.update(application_tree_peak_rss_bytes=peak_rss, application_tree_peak_pss_bytes=peak_pss, container_peak_bytes=memory_peak, container_limit_bytes=memory_limit, oom_kills=int(events['oom_kill']), network='none_except_container_loopback', normal_exit=True)
        finally:
            if application.poll() is None:
                os.killpg(application.pid, signal.SIGTERM)
                try:
                    application.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(application.pid, signal.SIGKILL)
                    application.wait(timeout=5)
args.report.write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report))
