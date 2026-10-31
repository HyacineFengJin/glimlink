#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Optimized empty-state core memory/GUI-dependency regression check.
Run in an isolated development container without account state mounted.
"""
import json
import pathlib
import subprocess
import sys
import tempfile

FORBIDDEN = ('webkit', 'javascriptcore', 'libgtk-', 'libv8', 'libblink', 'libchromium')

def stats(pid):
    status = pathlib.Path(f'/proc/{pid}/status').read_text()
    metrics = {}
    for line in status.splitlines():
        name, _, rest = line.partition(':')
        if name in ('VmRSS', 'VmHWM'):
            metrics[name] = int(rest.split()[0]) * 1024
    rollup = pathlib.Path(f'/proc/{pid}/smaps_rollup')
    if rollup.exists():
        for line in rollup.read_text().splitlines():
            if line.startswith('Pss:'):
                metrics['Pss'] = int(line.split()[1]) * 1024
    libraries = pathlib.Path(f'/proc/{pid}/maps').read_text().lower()
    assert not any(name in libraries for name in FORBIDDEN), 'GUI/browser library loaded into core'
    return metrics

def main():
    binary = sys.argv[1]
    with tempfile.TemporaryDirectory(prefix='agent-memory-') as state:
        process = subprocess.Popen([binary, '--state', state, 'operator', 'serve'],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            records = []
            for number, platform in enumerate(('bilibili', 'qq'), 1):
                request = {'version': 1, 'id': number, 'platform': platform,
                           'account': 'synthetic-memory', 'action': 'status', 'params': {}}
                process.stdin.write(json.dumps(request) + '\n')
                process.stdin.flush()
                response = json.loads(process.stdout.readline())
                assert response['id'] == number and response['ok']
                assert response['result']['status'] == 'signed_out'
                records.append(stats(process.pid))
            peak = max(record['VmHWM'] for record in records)
            assert peak <= 32 * 1024 * 1024, f'empty-state core RSS exceeded budget: {peak}'
            process.stdin.close()
            assert process.wait(timeout=5) == 0
            tools = subprocess.run([binary, 'tools'], capture_output=True, text=True, check=True)
            assert len(json.loads(tools.stdout)['tools']) == 32
            result = {'scope': 'optimized core, empty-account status and tool discovery',
                      'core_rss_peak_bytes': peak, 'rss_budget_bytes': 32 * 1024 * 1024,
                      'samples': records, 'gui_libraries_loaded': False,
                      'model_or_platform_requests': 0, 'private_state_used': False}
            peak_file = pathlib.Path('/sys/fs/cgroup/memory.peak')
            if peak_file.exists():
                result['test_container_memory_peak_bytes'] = int(peak_file.read_text())
            print(json.dumps(result, sort_keys=True))
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)

if __name__ == '__main__':
    main()
