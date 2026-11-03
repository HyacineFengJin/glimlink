#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Synthetic UI acceptance using the real optional system-WebKit window.

Run in the isolated operator-webview image with Xvfb and private D-Bus.
No account state, remote service, or native Runtime is used by the fixture.
"""
import argparse
import ctypes
import http.server
import importlib.util
import json
from pathlib import Path
import secrets
import subprocess
import threading
import time

parser = argparse.ArgumentParser()
parser.add_argument('--window-helper', default='/cache/bin/agent-login-window')
parser.add_argument('--frontend', type=Path, default=Path('/workspace/operator-desktop/frontend'))
parser.add_argument('--tests', type=Path, default=Path('/workspace/operator-desktop/tests/frontend.js'))
parser.add_argument('--report', type=Path, default=Path('/reports/operator-frontend-dom.json'))
args = parser.parse_args()
nonce = secrets.token_hex(24)
mock = """<script>
window.__errors=[];window.addEventListener('error',e=>window.__errors.push(e.message));
window.__calls=[];window.__maxConcurrent=0;window.__concurrent=0;window.__job=0;window.__memoryFailure=false;
localStorage.setItem('glimlink.operator.language','zh-CN');
window.__console={preferences:{revision:0,selected_agent:'main',modes:{}},profiles:{main:{revision:1,engine:'codex',model:'gpt-6.1-terra'}},agents:['main'],jobs:[],conversations:[],host:{status:'stopped'},worker_online:false,activities:{activities:[]}};
window.__commandConfig={revision:0,config:{enabled:false,bindings:[]},receipts:[]};
window.__TAURI__={event:{listen:async(name,callback)=>{window.__loginHandler=callback;return()=>{}}},core:{invoke:async(command,args)=>{
  window.__calls.push({command,args});const tracked=['platform_capabilities','account_status'].includes(command);if(tracked){window.__concurrent++;window.__maxConcurrent=Math.max(window.__maxConcurrent,window.__concurrent);}
  await new Promise(r=>setTimeout(r,2));if(tracked)window.__concurrent--;
  if(command==='launch_connection')return null;
  if(command==='ime_capabilities')return true;
  if(command==='ime_cursor_area')return null;
  if(command==='window_action'){if(args.action==='toggle_maximize')window.__maximized=!window.__maximized;return {maximized:!!window.__maximized,can_minimize:false};}
  if(command==='control_request'){
    const c=args.command,s=window.__console;
    if(c.action==='connections')return {identities:[{platform:'bilibili',account:'main',name:'连接',status:'online'}],cached_display_only:true};
    if(c.action==='snapshot')return JSON.parse(JSON.stringify(s));
    if(c.action==='conversation')return {history:{turns:[],truncated:false},jobs:s.jobs.filter(j=>j.context.persona===c.persona&&j.context.conversation===c.conversation),bounded:true};
    if(c.action==='send'){if(window.__holdSends)await new Promise(resolve=>(window.__releaseSends||=[]).push(resolve));if(window.__rejectSend){window.__rejectSend=false;throw {code:'control_unavailable'};}let j=s.jobs.find(j=>j.id===c.id);if(!j){j={id:c.id,user:c.text,context:{agent:c.agent,persona:c.persona,conversation:c.conversation},status:'pending',created_at:1,updated_at:1,model:s.profiles[c.agent].model};s.jobs.push(j)}if(window.__loseSendAck){window.__loseSendAck=false;throw {code:'control_timeout'};}return {job:j,queued:true};}
    if(c.action==='select'){s.preferences.selected_agent=c.agent;return {selected_agent:c.agent};}
    if(c.action==='mode'){s.preferences.modes[c.agent]=c.mode;return {applies_after_current_turn:true};}
    if(c.action==='profile'){const old=s.profiles[c.agent];if((old?.revision||0)!==c.expected_revision)throw {code:'revision_conflict'};s.profiles[c.agent]={revision:(old?.revision||0)+1,engine:c.engine,model:c.model};s.agents=Object.keys(s.profiles);return {profile:s.profiles[c.agent]};}
    if(c.action==='commands_get')return JSON.parse(JSON.stringify(window.__commandConfig));
    if(c.action==='commands_set'){if(c.expected_revision!==window.__commandConfig.revision)throw {code:'revision_conflict'};window.__commandConfig={revision:c.expected_revision+1,config:c.config,receipts:[]};return JSON.parse(JSON.stringify(window.__commandConfig));}
    if(c.action==='cancel'){const j=s.jobs.find(j=>j.id===c.id);if(j)j.status='canceled';return {canceled:true};}
    return {};
  }
  if(command==='worker_control'){window.__console.worker_online=args.start;window.__console.host=args.start?{phase:'waiting'}:{status:'stopped'};return {ok:true,running:args.start};}
  if(command==='memory_snapshot'){if(window.__memoryFailure)throw new Error('synthetic sampling failure');return {available:true,running:true,scope:'runtime_container',current_bytes:20971520,peak_bytes:33554432,limit_bytes:536870912,sampled_at_unix_ms:Date.now()};}
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
for name, mime in [('app.js', 'application/javascript'), ('console.js','application/javascript'),('locale.js','application/javascript'),('selects.js','application/javascript'),('window.js','application/javascript'),('input.js','application/javascript'),('styles.css', 'text/css'),('glimlink.svg', 'image/svg+xml')]:
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
    # Xvfb has no window manager to activate GTK windows. Give the synthetic
    # helper real X focus so requestAnimationFrame/IME geometry runs normally.
    spec = importlib.util.spec_from_file_location('desktop_acceptance', Path(__file__).with_name('validate-operator-desktop.py'))
    desktop = importlib.util.module_from_spec(spec); spec.loader.exec_module(desktop)
    display = desktop.Display()
    display.x.XSetInputFocus.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        match = next((wid for wid, title in display.windows() if title == 'Synthetic frontend acceptance'), None)
        if match:
            display.x.XSetInputFocus(display.display, match, 2, 0); display.x.XFlush(display.display)
            break
        time.sleep(0.01)
    else:
        raise AssertionError('synthetic window did not map')
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
