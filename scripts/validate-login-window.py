#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Synthetic WebView rendering/lifecycle acceptance; never connects a platform.
Run inside network-disabled Docker with the optional WebKit image and Xvfb.
"""
import http.server
import json
import os
import selectors
import subprocess
import sys
import threading
import time

NONCE = 'a' * 48
loaded = threading.Event()

class Fixture(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass
    def do_GET(self):
        if self.path != '/' + NONCE:
            self.send_error(404)
            return
        html = ('<!doctype html><html><meta charset=utf-8><title>Synthetic login window</title>'
                '<h1>Agent Runtime synthetic login</h1><p>No account or platform traffic.</p>'
                '<script>fetch(location.pathname+"/loaded",{method:"POST",body:"synthetic"})</script></html>').encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(html)))
        self.end_headers()
        self.wfile.write(html)
    def do_POST(self):
        if self.path != '/' + NONCE + '/loaded' or self.rfile.read(int(self.headers.get('Content-Length', '0'))) != b'synthetic':
            self.send_error(400)
            return
        loaded.set()
        self.send_response(200)
        self.send_header('Content-Length', '0')
        self.end_headers()

def event(process, timeout=10):
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        if not selector.select(timeout):
            raise AssertionError('window event deadline exceeded')
        line = process.stdout.readline(4097)
        assert line and len(line) <= 4096
        result = json.loads(line)
        assert result['version'] == 1
        assert not {'url', 'ticket', 'token', 'message'} & result.keys()
        return result

def launch(binary, url, lifetime=10, env=None):
    process = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, env=env, bufsize=0)
    process.stdin.write((json.dumps({'version': 1, 'action': 'open', 'url': url,
                                    'title': 'Synthetic login acceptance', 'timeout_secs': lifetime}) + '\n').encode())
    process.stdin.flush()
    return process

def stop(process):
    if process.stdin and not process.stdin.closed:
        process.stdin.close()
    if process.poll() is None:
        process.kill()
    process.wait(timeout=5)

def main():
    binary = sys.argv[1]
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    url = f'http://127.0.0.1:{server.server_port}/{NONCE}'
    result = {'synthetic_only': True, 'platform_requests': 0, 'account_state_used': False}
    process = launch(binary, url)
    try:
        assert event(process)['event'] == 'ready'
        assert loaded.wait(15), 'actual WebKit page JavaScript did not run'
        process.stdin.close()
        assert event(process)['event'] == 'closed'
        assert process.wait(timeout=5) == 0
        result['real_webkit_html_and_javascript'] = True
        result['owner_eof_closes_window'] = True
    finally:
        stop(process)
    process = launch(binary, url, lifetime=1)
    try:
        assert event(process)['event'] == 'ready'
        assert event(process)['code'] == 'expired'
        assert event(process)['event'] == 'closed'
        assert process.wait(timeout=5) == 1
        result['idle_window_expires'] = True
    finally:
        stop(process)
    env = dict(os.environ)
    env.pop('DISPLAY', None)
    env.pop('WAYLAND_DISPLAY', None)
    process = launch(binary, url, env=env)
    try:
        assert event(process)['code'] == 'no_display'
        assert process.wait(timeout=5) == 1
        result['no_display_is_explicit'] = True
    finally:
        stop(process)
    process = launch(binary, 'https://accounts.google.com/o/oauth2/auth')
    try:
        assert event(process)['code'] == 'invalid_input'
        assert process.wait(timeout=5) == 1
        result['remote_oauth_is_not_embedded'] = True
    finally:
        stop(process)
    server.shutdown()
    print(json.dumps(result, sort_keys=True))

if __name__ == '__main__':
    main()
