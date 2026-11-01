#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""External Docker/cgroup sampler. The monitor is not charged to the workload.

Opt-in Rust workloads run inside containers. Explicit interaction trials permit bounded
operator-authorized Bilibili participation; other cases are read-only. QQ/Weixin
are excluded. Raw logs and reports stay in the ignored private runtime directory.
"""
import argparse
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import selectors
import shutil
import subprocess
import time
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parents[1]
IMAGE = 'rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f'
OFFLINE = ['idle', 'state-loaded', 'image-small', 'image-photo', 'image-large', 'image-reject', 'gif', 'speech', 'mixed-loop']
ONLINE = ['video-detail', 'codex-detail', 'autonomous-life', 'autonomous-more', 'comment-trial', 'conversation-trial', 'home-feed', 'codex-browse', 'antigravity-browse', 'platform-sequential', 'platform-parallel', 'bili-video', 'codex-text', 'antigravity-text', 'codex-image', 'codex-video', 'codex-platforms', 'antigravity-platforms', 'codex-use', 'antigravity-use', 'codex-watch', 'antigravity-watch', 'codex-speech', 'antigravity-speech', 'codex-vision', 'antigravity-vision', 'codex-combined', 'antigravity-combined', 'engine-switch']

def command(args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()

def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

def read_numbers(path):
    try:
        return {parts[0]: int(parts[1]) for line in path.read_text().splitlines() if len(parts := line.split()) == 2}
    except (OSError, ValueError):
        return {}

def process_sample(pid):
    base = Path('/proc') / str(pid)
    try:
        status = base.joinpath('status').read_text()
        name = re.search(r'^Name:\s*(.*)$', status, re.M).group(1)
        rss = int(re.search(r'^VmRSS:\s*(\d+)', status, re.M).group(1)) * 1024
    except (OSError, AttributeError, ValueError):
        return None
    try:
        rollup = base.joinpath('smaps_rollup').read_text()
        def kilobytes(field):
            found = re.search(r'^' + field + r':\s*(\d+)', rollup, re.M)
            return int(found.group(1)) * 1024 if found else None
        pss, pss_anon, pss_file = (kilobytes(field) for field in ('Pss','Pss_Anon','Pss_File'))
    except (OSError, AttributeError, ValueError):
        pss = pss_anon = pss_file = None
    return dict(pid=pid, name=name, rss=rss, pss=pss, pss_anon=pss_anon, pss_file=pss_file)

def evict_experiment_files(paths):
    # Only private copies, never the host's caches or shared original binaries.
    for path in paths:
        with path.open('rb') as stream:
            os.fsync(stream.fileno())
            os.posix_fadvise(stream.fileno(), 0, 0, os.POSIX_FADV_DONTNEED)

def functional(value):
    if not value or not value.get('completed'):
        return False
    result = value.get('result') or {}
    case = value.get('case', '')
    if case in ('speech', 'mixed-loop'):
        speeches = [s for s in result.get('steps', []) if s.get('media_kind') == 'audio']
        if len(speeches) != (4 if case == 'mixed-loop' else 1) or any(s.get('speech_available') is not True or not s.get('speech_chars') for s in speeches):
            return False
    platforms = result.get('platforms')
    if platforms is not None:
        if len(platforms) != 4 or any(not p.get('clean_exit') or p.get('first', {}).get('status') != 'ok' or p.get('second', {}).get('status') != 'ok' for p in platforms):
            return False
    if isinstance(result.get('model'), dict):
        if result['model'].get('parallel_wave_during_tool') is not True or result['model'].get('pending_jobs') != 32:
            return False
        result = result['model']
    if case=='video-detail':
        return result.get('status')=='ok' and result.get('source_upgraded') is True and result.get('detail_preserved') is True and bool(result.get('sent_images')) and all(not a.get('resized_for_wire') for a in result['sent_images'])
    if 'engine' in result:
        if case.endswith('-detail'):
            details=[t for t in result.get('tool_trace',[]) if t.get('success') and t.get('region_requested') and (t.get('source_dimensions') or {}).get('width',0)>=1280 and t.get('attached_images')]
            if not details or any(a.get('resized_for_wire') for t in details for a in t.get('sent_images') or []):
                return False
        if case.endswith('-browse'):
            trace=result.get('tool_trace',[])
            candidates={video for step in trace if step.get('success') and step.get('method')=='social.discover' and step.get('feed_source')=='bilibili_home_recommendations' for video in step.get('candidate_ids') or []}
            windows=[step for step in trace if step.get('success') and step.get('method')=='media.watch' and step.get('prepared')]
            if not candidates or not windows or len(windows)>2 or any(step.get('video_id') not in candidates or not step.get('attached_images') for step in windows):
                return False
            if sum(step.get('requested_duration_ms') or 10000 for step in windows)>20000:
                return False
        if case.endswith('-vision') and result.get('vision_verified') is not True:
            return False
        if case.endswith('-speech') and not any(step.get('success') and step.get('media_kind') == 'audio' and step.get('speech_available') and step.get('speech_chars', 0) > 0 for step in result.get('tool_trace', [])):
            return False
        if case.endswith(('-use','-watch')):
            trace = result.get('tool_trace', [])
            trace = [step for step in trace if step.get('success')]
            if not trace:
                return False
            if case.endswith('-use'):
                platforms = {step.get('platform') for step in trace}
                if not {'bilibili','tieba','x','github'} <= platforms or not any(step['method'] == 'github.open' and step.get('kind') == 'file' for step in trace):
                    return False
            elif not any(step['method'] == 'media.watch' and step.get('prepared') and step.get('sampled_frames', 0) > 0 and step.get('attached_images', 0) > 0 for step in trace):
                return False
        minimum = 8 if case.endswith('-platforms') else 2 if case.endswith('-video') else 1
        return not result.get('provider_quota_notice') and result.get('status') == 'completed' and result.get('tool_calls', 0) >= minimum and result.get('operation_count') == 0 and result.get('model_reply_present') is True
    if case == 'conversation-trial':
        turns=result.get('turns',[])
        allowance=(result.get('activities',{}).get('activities') or [{}])[0].get('interaction_allowance') or {}
        return (len(turns)==1 and result.get('operator_directed_conversation_trial') is True
                and turns[0].get('status')=='completed' and allowance.get('reserved_comments',999)<=3
                and any(t.get('method')=='social.notifications' and t.get('success') for t in turns[0].get('tool_trace',[]))
                and not any(turns[0].get('action_evidence',{}).get('counts',{}).get(k,0) for k in ('unknown','not_recorded','prepared','invalid_record')))
    if case == 'comment-trial':
        turns=result.get('turns',[])
        allowance=(result.get('activities',{}).get('activities') or [{}])[0].get('interaction_allowance') or {}
        return (len(turns)==2 and result.get('operator_directed_comment_trial') is True
                and all(t.get('status')=='completed' for t in turns)
                and allowance.get('reserved_comments',999)<=2
                and not any(any(t.get('action_evidence',{}).get('counts',{}).get(k,0) for k in ('unknown','not_recorded','prepared','invalid_record')) for t in turns))
    if case == 'autonomous-more':
        turns=result.get('turns', [])
        activity=(result.get('activities', {}).get('activities') or [{}])[0]
        allowance=activity.get('interaction_allowance') or {}
        # An autonomous wake may end without a person-facing/private report.
        # HD, cropping, notes and public participation are observations, not quotas.
        return (len(turns)==4 and all(t.get('status')=='completed' and t.get('no_operator_prompt') is True for t in turns)
                and all(b.get('checkpoint_revision_before')==a.get('checkpoint_revision_after') and b.get('injected_prior_episode_count',0)>0 for a,b in zip(turns,turns[1:]))
                and allowance.get('reserved_likes',999)<=4 and allowance.get('reserved_comments',999)<=2
                # A known server rejection remains action evidence, not a failed
                # model lifecycle. Unknown/unrecorded writes are unresolved.
                and not any(any(t.get('action_evidence',{}).get('counts',{}).get(k,0)
                                for k in ('unknown','not_recorded','prepared','invalid_record')) for t in turns))
    if case == 'autonomous-life':
        turns=result.get('turns', [])
        return (len(turns)==2 and all(t.get('status')=='completed' and t.get('no_operator_prompt') is True and t.get('model_reply_present') and t.get('operation_count')==0 for t in turns)
                and turns[1].get('checkpoint_revision_before')==turns[0].get('checkpoint_revision_after') and turns[1].get('injected_prior_episode_count',0)>0)
    if 'turns' in result:
        return all(not t.get('provider_quota_notice') and t.get('status') == 'completed' and t.get('tool_calls', 0) > 0 for t in result['turns'])
    return result.get('status') in ('ok', 'expected_rejection')

def measure(case, args, suite, programs, models, cold_files):
    run_id = args.run_id
    label = f'glimlink-{run_id}-{case}-{args.budget}'
    status_file = f'/reports/{run_id}/{case}-exit.json'
    wrapper = '''umask 077
"$1" "$2" "$3"
rc=$?
printf '{"exit_code":%s}\n' "$rc" > "$4"
sleep 3
exit "$rc"
'''
    online = case in ONLINE
    create = ['docker', 'create', '--name', label, '--init', '--read-only', '--cap-drop', 'ALL',
              '--security-opt', 'no-new-privileges:true', '--user', f'{os.getuid()}:{os.getgid()}',
              '--memory', f'{args.budget}m', '--memory-swap', f'{args.budget}m', '--cpus', '2',
              '--pids-limit', '512', '--ulimit', 'core=0', '--ulimit', 'nofile=1024:1024',
              '--network', 'bridge' if online else 'none', '--cgroupns', 'private',
              '--tmpfs', '/tmp:rw,nosuid,nodev,noexec,size=128m',
              '--mount', f'type=bind,src={ROOT / ".runtime/cache"},dst=/cache,readonly',
              '--mount', f'type=bind,src={ROOT / ".runtime/reports"},dst=/reports',
              '--mount', f'type=bind,src={models},dst=/bench-model,readonly',
              '--env', 'HOME=/tmp', '--env', 'AGENT_RUNTIME_SPEECH_DIR=/bench-model',
              '--workdir', '/tmp']
    for option, variable in [(args.codex_model, 'GLIMLINK_BENCH_CODEX_MODEL'), (args.antigravity_model, 'GLIMLINK_BENCH_ANTIGRAVITY_MODEL')]:
        if option:
            create += ['--env', f'{variable}={option}']
    if online:
        create += ['--mount', f'type=bind,src={ROOT / ".runtime/state"},dst=/state']
    else:
        create += ['--tmpfs', f'/state:rw,nosuid,nodev,noexec,mode=0700,uid={os.getuid()},gid={os.getgid()},size=96m']
    if args.antigravity_go_memory_mib:
        create += ['--env', f'GLIMLINK_BENCH_AGY_GOMEM_MIB={args.antigravity_go_memory_mib}']
    for path in programs.iterdir():
        create += ['--mount', f'type=bind,src={path},dst=/cache/bin/{path.name},readonly']
    create += [IMAGE, '/bin/sh', '-c', wrapper, '--', '/cache/bin/runtime-memory-workload', case, run_id, status_file]
    identifier = command(create)
    if not args.warm:
        evict_experiment_files(cold_files)
    print(json.dumps(dict(event='start', case=case, budget_mib=args.budget, cold_private_file_advice=not args.warm)), flush=True)
    process = None
    samples = []
    phase = 'starting'
    events = []
    started = time.monotonic()
    result = dict(case=case, budget_bytes=args.budget * 1048576)
    try:
        command(['docker', 'start', identifier])
        init = int(command(['docker', 'inspect', '--format', '{{.State.Pid}}', identifier]))
        hierarchy = next(line.split(':', 2)[2] for line in Path(f'/proc/{init}/cgroup').read_text().splitlines() if line.startswith('0::'))
        cgroup = Path('/sys/fs/cgroup') / hierarchy.lstrip('/')
        if not (cgroup / 'memory.current').exists():
            raise RuntimeError('Host cgroup counters unavailable; do not substitute host memory')
        assert int((cgroup / 'memory.max').read_text()) == args.budget * 1048576
        process = subprocess.Popen(['docker', 'logs', '-f', identifier], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, bufsize=0)
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        log_buffer = b''
        with (suite / f'{case}-samples.jsonl').open('w') as traces:
            while time.monotonic() - started < args.timeout:
                for key, _ in selector.select(timeout=.1):
                    chunk = os.read(key.fileobj.fileno(), 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    log_buffer += chunk
                    while b'\n' in log_buffer:
                        line, log_buffer = log_buffer.split(b'\n', 1)
                        try:
                            event = json.loads(line)
                        except ValueError:
                            continue
                        if event.get('event') == 'phase':
                            phase = event['name']
                            event['seconds'] = time.monotonic() - started
                            events.append(event)
                            print(json.dumps(dict(event='phase', case=case, name=phase)), flush=True)
                    if len(log_buffer) > 1048576:
                        raise RuntimeError('Workload log exceeded its line bound')
                try:
                    current = int((cgroup / 'memory.current').read_text())
                    peak = int((cgroup / 'memory.peak').read_text())
                    pids = [int(p) for p in (cgroup / 'cgroup.procs').read_text().split()]
                    processes = [item for pid in pids if (item := process_sample(pid))]
                    stat = read_numbers(cgroup / 'memory.stat')
                    sample = dict(seconds=round(time.monotonic() - started, 3), phase=phase,
                                  current=current, peak=peak, anon=stat.get('anon'), file=stat.get('file'),
                                  kernel=stat.get('kernel'), file_mapped=stat.get('file_mapped'), file_dirty=stat.get('file_dirty'), file_writeback=stat.get('file_writeback'), rss=sum(p['rss'] for p in processes),
                                  pss_anon=sum(p['pss_anon'] or 0 for p in processes), pss_file=sum(p['pss_file'] or 0 for p in processes),
                                  pss_breakdown_complete=all(p['pss_anon'] is not None and p['pss_file'] is not None for p in processes),
                                  pss=sum(p['pss'] or 0 for p in processes), pss_complete=all(p['pss'] is not None for p in processes),
                                  processes=processes, memory_events=read_numbers(cgroup / 'memory.events'))
                    samples.append(sample)
                    traces.write(json.dumps(sample) + '\n')
                except (FileNotFoundError, ProcessLookupError):
                    break
                if process.poll() is not None:
                    break
            else:
                result['timed_out'] = True
                command(['docker', 'kill', identifier], stderr=subprocess.DEVNULL)
        selector.close()
        process.wait(timeout=10)
        container = json.loads(command(['docker', 'inspect', identifier]))[0]
        value_file = suite / f'{case}-result.json'
        value = json.loads(value_file.read_text()) if value_file.exists() else None
        if samples:
            final_events = samples[-1]['memory_events']
            maximum = max(samples, key=lambda s: s['current'])
            result.update(cgroup_peak_bytes=max(s['peak'] for s in samples), sampled_rss_peak_bytes=max(s['rss'] for s in samples),
                          sampled_pss_peak_bytes=max(s['pss'] for s in samples), anon_peak_bytes=max(s['anon'] or 0 for s in samples),
                          file_peak_bytes=max(s['file'] or 0 for s in samples), peak_phase=maximum['phase'],
                          sample_count=len(samples), pss_all_samples_complete=all(s['pss_complete'] for s in samples),
                          memory_events=final_events, process_names=sorted({p['name'] for s in samples for p in s['processes']}))
            result['phase_residency'] = {name: dict(pss_min_bytes=min(s['pss'] for s in samples if s['phase'] == name),
                pss_max_bytes=max(s['pss'] for s in samples if s['phase'] == name),
                cgroup_max_bytes=max(s['current'] for s in samples if s['phase'] == name)) for name in {s['phase'] for s in samples}}
        result.update(elapsed_seconds=round(time.monotonic() - started, 2), exit_code=container['State']['ExitCode'],
                      oom_killed=container['State']['OOMKilled'], workload=value,
                      functionality_completed=functional(value), phase_events=events)
        (suite / f'{case}-memory.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(dict(event='complete', case=case, peak_mib=round(result.get('cgroup_peak_bytes', 0) / 1048576, 1),
                              pss_mib=round(result.get('sampled_pss_peak_bytes', 0) / 1048576, 1),
                              passed=result['functionality_completed'], exit_code=result['exit_code'],
                              oom_kills=result.get('memory_events', {}).get('oom_kill', 0))), flush=True)
        return result
    finally:
        if process and process.poll() is None:
            process.terminate()
            process.wait(timeout=5)
        subprocess.run(['docker', 'rm', '-f', identifier], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cases', default=','.join(OFFLINE + [c for c in ONLINE if c not in ('autonomous-more','comment-trial','conversation-trial')]))
    parser.add_argument('--budget', type=int, default=384)
    parser.add_argument('--run-id', default='mem-' + secrets.token_hex(5))
    parser.add_argument('--timeout', type=int, default=420)
    parser.add_argument('--warm', action='store_true')
    parser.add_argument('--allow-bilibili-interactions', action='store_true', help='Explicitly authorize bounded Bilibili participation: autonomous-more allows 4 likes/2 comments; comment-trial allows 2 comments; conversation-trial adds 1 reply')
    parser.add_argument('--codex-model')
    parser.add_argument('--antigravity-model')
    parser.add_argument('--antigravity-go-memory-mib', type=int)
    parser.add_argument('--program-dir', type=Path)
    args = parser.parse_args()
    assert 128 <= args.budget <= 1024 and re.fullmatch(r'[A-Za-z0-9-]{1,32}', args.run_id)
    assert args.antigravity_go_memory_mib is None or 32 <= args.antigravity_go_memory_mib <= 256
    assert all(model is None or re.fullmatch(r'[A-Za-z0-9._:-]{1,128}', model) for model in [args.codex_model,args.antigravity_model])
    cases = args.cases.split(',')
    assert not {'autonomous-more','comment-trial','conversation-trial'}.intersection(cases) or args.allow_bilibili_interactions, 'interaction trials require explicit --allow-bilibili-interactions'
    assert all(case in OFFLINE + ONLINE for case in cases)
    os.umask(0o077)
    suite = ROOT / '.runtime/reports' / args.run_id
    suite.mkdir(mode=0o700)
    private = ROOT / '.runtime/cache/memory-programs' / args.run_id
    programs = private / 'bin'
    models = private / 'models'
    programs.mkdir(parents=True, mode=0o700)
    models.mkdir(mode=0o700)
    for name in ['runtime-memory-workload', 'agent-runtime', 'codex-native', 'antigravity-native']:
        shutil.copyfile((args.program_dir or ROOT / '.runtime/cache/bin') / name, programs / name)
        (programs / name).chmod(0o700)
    for name in ['model-tiny-q80.gguf', 'tokenizer-tiny.json', 'config-tiny.json', 'melfilters.bytes']:
        shutil.copyfile(ROOT / '.runtime/cache/models/whisper-tiny-q80' / name, models / name)
    cold_files = list(programs.iterdir()) + list(models.iterdir())
    report = dict(schema=1, run_id=args.run_id, measured_at=datetime.now(ZoneInfo('Asia/Shanghai')).isoformat(),
                  monitor_outside_container=True, sample_interval_ms=100, memory_swap_disabled=True,
                  program_sha256={p.name: digest(p) for p in programs.iterdir()},
                  requested_models={'codex':args.codex_model,'antigravity':args.antigravity_model},
                  requested_antigravity_go_memory_mib=args.antigravity_go_memory_mib,
                  cold_file_advice=not args.warm, cold_scope='Private binary/model copies only; shared system libraries may remain warm. File advice is not a global cache flush.',
                  bounded_bilibili_participation_authorized=args.allow_bilibili_interactions, excluded_platforms=['qq', 'weixin'], cases=[])
    for case in cases:
        report['cases'].append(measure(case, args, suite, programs, models, cold_files))
        (suite / 'summary.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(dict(event='suite_complete', run_id=args.run_id, cases=len(report['cases']), report=str(suite / 'summary.json'))), flush=True)

if __name__ == '__main__':
    main()
