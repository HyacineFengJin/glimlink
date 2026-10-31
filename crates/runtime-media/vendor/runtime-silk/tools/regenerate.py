#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Explicit developer-only C2Rust regeneration inside the pinned tools image.
Usage: regenerate.py SDK_SILK_DIRECTORY NEW_OUTPUT_DIRECTORY
No downloads, accounts, runtime installation, or in-place source replacement.
"""
import hashlib,json,pathlib,shutil,subprocess,sys
if len(sys.argv)!=3:raise SystemExit(__doc__)
tools=pathlib.Path(__file__).resolve().parent
sdk=pathlib.Path(sys.argv[1]).resolve();out=pathlib.Path(sys.argv[2]).resolve()
if out.exists():raise SystemExit('output directory must not exist')
if not (sdk/'interface/SKP_Silk_SDK_API.h').is_file():raise SystemExit('not an SDK silk directory')
out.mkdir(parents=True);work=out/'source';work.mkdir()
for directory in ['src','interface']:shutil.copytree(sdk/directory,work/directory)
sources=json.loads((tools/'decoder-sources.json').read_text())
if len(sources)!=54 or not all((work/'src'/name).is_file() for name in sources):raise SystemExit('pinned decoder source closure is incomplete')
amalgam=work/'decoder_amalgam.c'
amalgam.write_text('#define NDEBUG 1\n'+''.join(f'#include "src/{name}"\n' for name in sources))
commands=[dict(directory=str(work),file=str(amalgam),arguments=['clang-15','-std=c99','-O0','-DNDEBUG','-fwrapv','-Iinterface','-Isrc','-c','decoder_amalgam.c','-o','decoder_amalgam.o'])]
(work/'compile_commands.json').write_text(json.dumps(commands))
subprocess.run(['c2rust','transpile',str(work/'compile_commands.json'),'-o',str(out/'generated'),'--overwrite-existing','--emit-modules','--translate-const-macros','conservative'],cwd=work,check=True)
raw=out/'generated/src/decoder_amalgam.rs';target=out/'kernel.rs'
subprocess.run([sys.executable,str(tools/'clean_generated.py'),str(raw),str(target)],check=True)
metadata=dict(source=json.loads((tools/'source-pin.json').read_text()),amalgam_sha256=hashlib.sha256(amalgam.read_bytes()).hexdigest(),raw_rust_sha256=hashlib.sha256(raw.read_bytes()).hexdigest(),kernel_sha256=hashlib.sha256(target.read_bytes()).hexdigest(),source_files=sources)
(out/'generation.json').write_text(json.dumps(metadata,indent=2)+'\n')
print(json.dumps({k:v for k,v in metadata.items() if k.endswith('sha256')},indent=2))
