#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Synthetic UI acceptance using the real optional system-WebKit window.

Run in the isolated operator-webview image with Xvfb and private D-Bus.
No account state, remote service, or native Runtime is used by the fixture.
"""
import argparse
import http.server
import json
from pathlib import Path
import secrets
import subprocess
import threading

parser = argparse.ArgumentParser()
parser.add_argument('--window-helper', default='/cache/bin/agent-login-window')
parser.add_argument('--frontend', type=Path, default=Path('/workspace/operator-desktop/frontend'))
parser.add_argument('--tests', type=Path, default=Path('/workspace/operator-desktop/tests/frontend.js'))
parser.add_argument('--report', type=Path, default=Path('/reports/operator-frontend-dom.json'))
args = parser.parse_args()
nonce = secrets.token_hex(24)
mock = """<script>
window.__calls=[];window.__maxConcurrent=0;window.__concurrent=0;window.__job=0;window.__memoryFailure=false;
window.__TAURI__={event:{listen:async(name,callback)=>{window.__loginHandler=callback;return()=>{}}},core:{invoke:async(command,args)=>{
  window.__calls.push({command,args});window.__concurrent++;window.__maxConcurrent=Math.max(window.__maxConcurrent,window.__concurrent);
  await new Promise(r=>setTimeout(r,2));window.__concurrent--;
  if(command==='memory_snapshot'){if(window.__memoryFailure)throw new Error('synthetic sampling failure');return {available:true,partial:false,scope:'local_operator_process_tree',rss_bytes:20971520,pss_bytes:10485760,process_count:3,sampled_at_unix_ms:Date.now()};}
  if(command==='begin_login')return {login_id:'synthetic-'+(++window.__job)};
  if(command==='platform_capabilities')return {platform:args.platform,methods:[],reuse:true,paused:args.platform==='weixin'};
  if(command==='account_status')return {status:'signed_out',online_checked:false};
  if(command==='cancel_login'){window.__loginHandler({payload:{login_id:args.loginId,platform:'bilibili',kind:'finished',data:{status:'authenticated',reused_session:true}}});return {status:'authenticated'}};
  return {ok:true};
}}};
</script>"""
source = (args.frontend / 'index.html').read_text().replace('<meta charset="utf-8">', '<meta charset="utf-8">' + mock, 1)
source = source.replace('<script src="app.js" defer></script>', '<script src="app.js" defer></script><script src="test.js" defer></script>', 1)
tests = args.tests.read_text().replace("textContent:JSON.stringify(results)}));", "textContent:JSON.stringify(results)}));fetch('/synthetic-result',{method:'POST',body:JSON.stringify(results)});")
assert "fetch('/synthetic-result'" in tests
routes = {'/' + nonce: (source.encode(), 'text/html; charset=utf-8'), '/test.js': (tests.encode(), 'application/javascript')}
for name, mime in [('app.js', 'application/javascript'), ('styles.css', 'text/css')]:
    routes['/' + name] = ((args.frontend / name).read_bytes(), mime)
for asset in (args.frontend / 'brands').iterdir():
    if asset.suffix in {'.ico', '.png', '.jpg', '.svg'}:
        mime = {'.ico':'image/x-icon','.png':'image/png','.jpg':'image/jpeg','.svg':'image/svg+xml'}[asset.suffix]
        routes['/brands/' + asset.name] = (asset.read_bytes(), mime)
finished = threading.Event()
result = None
class Fixture(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_GET(self):
        value = routes.get(self.path)
        if value is None:
            self.send_error(404); return
        data, mime = value
        self.send_response(200); self.send_header('Content-Type', mime); self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
    def do_POST(self):
        global result
        if self.path != '/synthetic-result':
            self.send_error(404); return
        length = int(self.headers.get('Content-Length', '0'))
        if not 0 < length <= 16384:
            self.send_error(400); return
        result = json.loads(self.rfile.read(length))
        self.send_response(200); self.send_header('Content-Length', '0'); self.end_headers(); finished.set()
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
threading.Thread(target=server.serve_forever, daemon=True).start()
window = subprocess.Popen([args.window_helper], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
try:
    window.stdin.write(json.dumps(dict(version=1, action='open', url=f'http://127.0.0.1:{server.server_port}/{nonce}', title='Synthetic frontend acceptance', timeout_secs=20)) + '\n'); window.stdin.flush()
    assert finished.wait(15), 'synthetic UI did not finish in the real WebKit window'
    args.report.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))
    assert result and 'error' not in result and all(value is True for value in result.values()), 'synthetic DOM acceptance failed'
finally:
    window.stdin.close()
    try:
        window.wait(timeout=5)
    except subprocess.TimeoutExpired:
        window.kill(); window.wait(timeout=5)
    server.shutdown(); server.server_close()
