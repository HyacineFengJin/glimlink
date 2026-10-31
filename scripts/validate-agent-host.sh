#!/usr/bin/env bash
# Isolated, account-free operator-CLI acceptance. Native Codex is only probed
# against a new empty CODEX_HOME, with network disabled; no model turn can run.
set -euo pipefail
umask 077
runtime_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
exec bash "$runtime_root/scripts/isolate.sh" run stdio -- python3 - <<'PY'
import json,os,subprocess,tempfile,time,resource
root=tempfile.mkdtemp(prefix='agent-host-acceptance-',dir='/state')
base=['/cache/bin/agent-runtime','--state',root]
steps=[]
def invoke(args):
    p=subprocess.run(base+['agent']+args,text=True,capture_output=True,timeout=45)
    values=[json.loads(line) for line in p.stdout.splitlines()]
    if p.returncode or not values: raise RuntimeError('operator command failed; no model was authorized')
    result=values[-1]
    steps.append({'args':args,'result':result})
    return result
idle=invoke(['run','--once','--binary','/missing/native-engine'])
assert idle['cycles']==1 and idle['model_process_active'] is False
prompt=os.path.join(root,'read-only-prompt.txt')
with open(prompt,'w') as f:f.write('Read Runtime module capabilities only. Do not send messages or modify any platform content.')
os.chmod(prompt,0o600)
invoke(['enqueue','native-auth-probe',prompt])
auth=invoke(['run','--once','--timeout-seconds','15'])
assert auth['status']=='needs_auth'
job=invoke(['job','native-auth-probe'])['job']
assert job['status']=='needs_auth' and job['progress']['tool_calls']==0 and job['progress']['thread_id'] is None
assert not os.path.exists(os.path.join(root,'codex','auth.json'))
invoke(['enqueue','canceled-before-model',prompt])
invoke(['cancel','canceled-before-model'])
assert invoke(['job','canceled-before-model'])['job']['status']=='canceled'
daemon=subprocess.Popen(base+['agent','run','--poll-ms','250'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
try:
    deadline=time.monotonic()+5
    while time.monotonic()<deadline:
        try:
            snapshot=json.load(open(os.path.join(root,'agent-status.json')))
            if snapshot.get('pid')==daemon.pid and snapshot.get('phase')=='waiting':break
        except (FileNotFoundError,json.JSONDecodeError):pass
        time.sleep(.02)
    else:raise RuntimeError('idle daemon heartbeat unavailable')
    status=invoke(['status'])
    assert status['jobs']['counts']['needs_auth']==1 and status['jobs']['counts']['canceled']==1
    assert status['sources']['items']==[]
    daemon.terminate()
    out,err=daemon.communicate(timeout=5)
    assert daemon.returncode==0
    final=[json.loads(line) for line in out.splitlines()][-1]
    assert final['status']=='stopped' and final['model_process_active'] is False
finally:
    if daemon.poll() is None:daemon.kill();daemon.communicate()
summary={'status':'passed','native_app_server_probed':True,'fresh_empty_codex_home':True,
    'real_model_turn_started':False,'platform_writes':0,'watches_enabled':0,
    'idle_without_engine_binary':True,'needs_auth_before_thread':True,'cancel_before_model':True,
    'management_status_during_daemon':True,'sigterm_shutdown_clean':True,
    'max_child_rss_kib':resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss,
    'cgroup_memory_peak_bytes':int(open('/sys/fs/cgroup/memory.peak').read()),
    'cgroup_memory_limit_bytes':int(open('/sys/fs/cgroup/memory.max').read())}
report={'summary':summary,'steps':steps,'state':root}
fd,path=tempfile.mkstemp(prefix='agent-host-native-acceptance-',suffix='.json',dir='/reports')
with os.fdopen(fd,'w') as f:json.dump(report,f,ensure_ascii=False);f.flush();os.fsync(f.fileno())
print(json.dumps({'report':path,**summary},ensure_ascii=False))
PY
