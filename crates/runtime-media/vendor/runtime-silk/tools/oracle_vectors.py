#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regenerate public synthetic bit-exact vectors using the pinned C SDK oracle.
Run inside a development compiler container; these tools never enter Runtime.
Usage: oracle_vectors.py SDK_SILK_DIRECTORY NEW_OUTPUT_DIRECTORY
"""
import hashlib,json,pathlib,subprocess,sys
if len(sys.argv)!=3:raise SystemExit(__doc__)
tools=pathlib.Path(__file__).resolve().parent;sdk=pathlib.Path(sys.argv[1]).resolve();out=pathlib.Path(sys.argv[2]).resolve()
if out.exists():raise SystemExit('output directory must not exist')
if not (sdk/'test/Encoder.c').is_file():raise SystemExit('not an SDK silk directory')
subprocess.run(['make','-j4','lib',"CFLAGS=-O2 -DNDEBUG -fwrapv -Iinterface -Isrc -Itest"],cwd=sdk,check=True)
subprocess.run(['make','-j4','encoder','decoder',"CFLAGS=-O2 -DNDEBUG -fwrapv -Iinterface -Isrc -Itest"],cwd=sdk,check=True)
subprocess.run([sys.executable,str(tools/'synthesize.py'),str(out)],check=True)
cases=json.loads((out/'cases.json').read_text());results=[]
for case in cases:
 name=case['name'];encoded=out/f'{name}.silk';reference=out/f'{name}.pcm'
 args=[str(sdk/'encoder'),str(out/f'{name}.input.pcm'),str(encoded),'-Fs_API',str(case['rate']),'-Fs_maxInternal',str(case['rate']),'-packetlength',str(case['packet_ms']),'-rate','18000','-quiet']
 if case['tencent']:args+=['-tencent']
 if case['dtx']:args+=['-DTX','1']
 subprocess.run(args,check=True);subprocess.run([str(sdk/'decoder'),str(encoded),str(reference),'-Fs_API','24000','-quiet'],check=True)
 results.append(dict(**case,silk_sha256=hashlib.sha256(encoded.read_bytes()).hexdigest(),pcm_sha256=hashlib.sha256(reference.read_bytes()).hexdigest(),samples=reference.stat().st_size//2))
for rate in [8000,12000,16000,24000,32000,44100,48000]:
 reference=out/f'mix16.rate{rate}.pcm';subprocess.run([str(sdk/'decoder'),str(out/'mix16.silk'),str(reference),'-Fs_API',str(rate),'-quiet'],check=True)
 results.append(dict(name=f'mix16.rate{rate}',output_rate=rate,pcm_sha256=hashlib.sha256(reference.read_bytes()).hexdigest(),samples=reference.stat().st_size//2))
(out/'oracle-results.json').write_text(json.dumps(dict(source=json.loads((tools/'source-pin.json').read_text()),results=results),indent=2)+'\n')
