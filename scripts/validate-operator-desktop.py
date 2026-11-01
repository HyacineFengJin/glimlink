#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Exercise the packaged desktop on an isolated Xvfb display, without accounts.

Run through `isolate.sh client-test operator-webview` and dbus-run-session.
No real desktop, state, network, or login is required. Memory includes the
application's WebKit subprocesses; the container also includes test utilities.
"""
import argparse
import ctypes as c
import json
import importlib.util
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

spec = importlib.util.spec_from_file_location('screenshot_operator', Path(__file__).with_name('screenshot-operator.py'))
screenshots = importlib.util.module_from_spec(spec)
spec.loader.exec_module(screenshots)


def processes():
    result = {}
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit():
            continue
        try:
            fields = (entry / 'stat').read_text().rsplit(')', 1)[1].split()
            status = (entry / 'status').read_text().splitlines()
            values = {line.split(':', 1)[0]: line.split(':', 1)[1].strip() for line in status}
            result[int(entry.name)] = (int(fields[1]), values.get('Name', ''), int(values.get('VmRSS', '0 kB').split()[0]) * 1024)
        except (OSError, ValueError, IndexError):
            continue
    return result


def owned_processes(root, table):
    owned = {root}
    while True:
        more = {pid for pid, (parent, _, _) in table.items() if parent in owned}
        expanded = owned | more
        if expanded == owned:
            return owned & table.keys()
        owned = expanded


def pss(pid):
    try:
        return sum(int(line.split()[1]) * 1024 for line in Path(f'/proc/{pid}/smaps_rollup').read_text().splitlines() if line.startswith('Pss:'))
    except OSError:
        return 0


class Display:
    def __init__(self):
        self.x = c.CDLL('libX11.so.6')
        self.x.XOpenDisplay.argtypes = [c.c_char_p]
        self.x.XOpenDisplay.restype = c.c_void_p
        self.display = self.x.XOpenDisplay(None)
        assert self.display, 'Xvfb is unavailable'
        self.x.XDefaultRootWindow.argtypes = [c.c_void_p]
        self.x.XDefaultRootWindow.restype = c.c_ulong
        self.root = self.x.XDefaultRootWindow(self.display)
        self.x.XQueryTree.argtypes = [c.c_void_p, c.c_ulong, c.c_void_p, c.c_void_p, c.c_void_p, c.c_void_p]
        self.x.XFree.argtypes = [c.c_void_p]
        self.x.XFlush.argtypes = [c.c_void_p]
        self.x.XFetchName.argtypes = [c.c_void_p, c.c_ulong, c.c_void_p]
        self.x.XInternAtom.argtypes = [c.c_void_p, c.c_char_p, c.c_int]
        self.x.XInternAtom.restype = c.c_ulong
        self.x.XGetWindowProperty.argtypes = [c.c_void_p, c.c_ulong, c.c_ulong, c.c_long, c.c_long, c.c_int, c.c_ulong, c.c_void_p, c.c_void_p, c.c_void_p, c.c_void_p, c.c_void_p]

    def title(self, window):
        property_type, format_ = c.c_ulong(), c.c_int()
        items, remaining = c.c_ulong(), c.c_ulong()
        data = c.c_void_p()
        atom = self.x.XInternAtom(self.display, b'_NET_WM_NAME', 0)
        result = self.x.XGetWindowProperty(self.display, window, atom, 0, 1024, 0, 0, c.byref(property_type), c.byref(format_), c.byref(items), c.byref(remaining), c.byref(data))
        if result == 0 and data.value:
            title = c.string_at(data, items.value).decode('utf-8', errors='replace') if format_.value == 8 else ''
            self.x.XFree(data)
            if title:
                return title
        name = c.c_char_p()
        if self.x.XFetchName(self.display, window, c.byref(name)) and name.value:
            title = name.value.decode('utf-8', errors='replace')
            self.x.XFree(name)
            return title
        return ''

    def windows(self):
        root, parent = c.c_ulong(), c.c_ulong()
        children = c.POINTER(c.c_ulong)()
        count = c.c_uint()
        assert self.x.XQueryTree(self.display, self.root, c.byref(root), c.byref(parent), c.byref(children), c.byref(count))
        result = []
        for i in range(count.value):
            result.append((children[i], self.title(children[i])))
        if children:
            self.x.XFree(children)
        return result

    def click(self, x, y):
        xtest = c.CDLL('libXtst.so.6')
        xtest.XTestFakeMotionEvent.argtypes = [c.c_void_p, c.c_int, c.c_int, c.c_int, c.c_ulong]
        xtest.XTestFakeButtonEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]
        xtest.XTestFakeMotionEvent(self.display, -1, x, y, 0)
        xtest.XTestFakeButtonEvent(self.display, 1, 1, 0)
        xtest.XTestFakeButtonEvent(self.display, 1, 0, 0)
        self.x.XFlush(self.display)

    def close_window(self, window):
        # ICCCM WM_DELETE_WINDOW asks Tauri to exit through its normal cleanup.
        class ClientMessage(c.Structure):
            _fields_ = [('type', c.c_int), ('serial', c.c_ulong), ('send_event', c.c_int), ('display', c.c_void_p), ('window', c.c_ulong), ('message_type', c.c_ulong), ('format', c.c_int), ('data', c.c_long * 5)]
        self.x.XSendEvent.argtypes = [c.c_void_p, c.c_ulong, c.c_int, c.c_long, c.c_void_p]
        message = ClientMessage(33, 0, 1, self.display, window, self.x.XInternAtom(self.display, b'WM_PROTOCOLS', 0), 32)
        message.data[0] = self.x.XInternAtom(self.display, b'WM_DELETE_WINDOW', 0)
        self.x.XSendEvent(self.display, window, 0, 0, c.byref(message))
        self.x.XFlush(self.display)

    def resize(self, window, width, height):
        self.x.XResizeWindow.argtypes = [c.c_void_p, c.c_ulong, c.c_uint, c.c_uint]
        self.x.XResizeWindow(self.display, window, width, height)
        self.x.XFlush(self.display)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary')
    parser.add_argument('--report', default='/reports/operator-desktop-native.json')
    parser.add_argument('--screenshot', default='/reports/operator-desktop-native.png')
    parser.add_argument('--inspect-click', nargs=2, type=int, metavar=('X', 'Y'))
    parser.add_argument('--window-size', nargs=2, type=int, metavar=('WIDTH', 'HEIGHT'))
    parser.add_argument('--launch-destination')
    args = parser.parse_args()
    if args.window_size:
        assert 780 <= args.window_size[0] <= 1920 and 620 <= args.window_size[1] <= 1600
    peak_rss = peak_pss = idle_peak_rss = idle_peak_pss = 0
    seen = set()
    report = None
    with tempfile.TemporaryDirectory(prefix='operator-desktop-test-') as temporary:
        home = Path(temporary)
        environment = dict(os.environ, HOME=temporary, XDG_CONFIG_HOME=str(home / 'config'), XDG_CACHE_HOME=str(home / 'cache'), XDG_DATA_HOME=str(home / 'data'), XDG_RUNTIME_DIR=str(home / 'run'), AGENT_RUNTIME_STATE=str(home / 'state'))
        (home / 'run').mkdir(mode=0o700)
        with open(str(args.report) + '.stderr', 'wb') as errors:
            command = [args.binary]
            if args.launch_destination:
                assert not args.inspect_click, 'preset acceptance must not contact a real SSH host'
                command.extend(['--ssh', args.launch_destination, '--runtime', '/opt/glimlink/runtime', '--state', '/state', '--account', 'main'])
            application = subprocess.Popen(command, cwd=temporary, env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=errors, start_new_session=True)
            display = Display()
            window = None
            start = time.monotonic()
            clicked = False
            try:
                while time.monotonic() - start < (20 if args.inspect_click else 10):
                    assert application.poll() is None, f'desktop exited early: {application.returncode}'
                    table = processes()
                    owned = owned_processes(application.pid, table)
                    seen.update(table[pid][1] for pid in owned)
                    sample_rss = sum(table[pid][2] for pid in owned)
                    sample_pss = sum(pss(pid) for pid in owned)
                    peak_rss = max(peak_rss, sample_rss)
                    peak_pss = max(peak_pss, sample_pss)
                    if not clicked:
                        idle_peak_rss = max(idle_peak_rss, sample_rss)
                        idle_peak_pss = max(idle_peak_pss, sample_pss)
                    if window is None:
                        window = next((wid for wid, title in display.windows() if 'Glimlink' in title), None)
                        if window and args.window_size:
                            display.resize(window, *args.window_size)
                    if args.inspect_click and not clicked and window and time.monotonic() - start > 7:
                        display.click(*args.inspect_click)
                        clicked = True
                    time.sleep(.05)
                screenshots.capture(args.screenshot)
                assert window, f'native application window did not appear; X11 windows: {display.windows()!r}; processes: {sorted(seen)!r}'
                display.close_window(window)
                application.wait(timeout=10)
                assert application.returncode == 0, f'desktop exit code: {application.returncode}'
                cgroup = Path('/sys/fs/cgroup')
                memory_peak = int((cgroup / 'memory.peak').read_text())
                memory_max = (cgroup / 'memory.max').read_text().strip()
                memory_events = dict(line.split() for line in (cgroup / 'memory.events').read_text().splitlines())
                assert memory_max == str(1024**3), 'run with the 1 GiB client-test budget'
                assert memory_peak < 1024**3 and int(memory_events['oom_kill']) == 0
                if args.launch_destination:
                    assert 'ssh' not in seen, 'connection preset must not connect on startup'
                if clicked:
                    assert 'agent-runtime' in seen, 'GUI click did not invoke the real Runtime IPC'
                report = dict(native_tauri_window=True, bundled_frontend=True, gui_status_ipc_observed=clicked and 'agent-runtime' in seen, fresh_synthetic_state=True, network='none', real_account_used=False, application_tree_peak_rss_bytes=peak_rss, application_tree_peak_pss_bytes=peak_pss, idle_application_tree_peak_rss_bytes=idle_peak_rss, idle_application_tree_peak_pss_bytes=idle_peak_pss, container_peak_bytes=memory_peak, container_limit_bytes=int(memory_max), oom_kills=int(memory_events['oom_kill']), normal_window_close=True, observed_process_names=sorted(seen), screenshot=args.screenshot)
            finally:
                if application.poll() is None:
                    os.killpg(application.pid, signal.SIGTERM)
                    try:
                        application.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(application.pid, signal.SIGKILL)
                        application.wait(timeout=5)
        Path(args.report).write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(report))


if __name__ == '__main__':
    main()
