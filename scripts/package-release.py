#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Stage a reviewed source allowlist and assemble Linux release artifacts.

Development-machine tool only. Never copies account/cache roots or native agents.
The source artifact includes cargo-vendored dependencies for corresponding source.
"""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]

def sha(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()

def archive(source, destination):
    # Stable archive owners, permissions and timestamps; never preserve host paths.
    with destination.open('wb') as f, gzip.GzipFile(filename='', mode='wb', fileobj=f, mtime=0) as z:
        with tarfile.open(fileobj=z, mode='w') as tar:
            for path in [source, *sorted(source.rglob('*'))]:
                if path.is_symlink():
                    raise RuntimeError('Symlink in release staging: ' + str(path))
                info = tar.gettarinfo(path, arcname=path.relative_to(source.parent).as_posix())
                info.uid = info.gid = 0
                info.uname = info.gname = ''
                info.mtime = 0
                if path.is_file():
                    info.mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
                    with path.open('rb') as content:
                        tar.addfile(info, content)
                else:
                    info.mode = 0o755
                    tar.addfile(info)

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('action', choices=['stage', 'pack'])
    p.add_argument('--stage', type=Path, required=True)
    p.add_argument('--binaries', type=Path)
    p.add_argument('--out', type=Path)
    a = p.parse_args()
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']
    stage = a.stage.resolve()
    if not stage.is_relative_to(ROOT / '.runtime'):
        raise SystemExit('Staging must be inside the ignored .runtime directory.')
    source = stage / ('glimlink-' + version + '-source')
    if a.action == 'stage':
        if stage.exists():
            raise SystemExit('Use a new, empty staging path.')
        audit = json.loads(subprocess.check_output(['python3', str(ROOT / 'scripts/source-release-audit.py')], text=True))
        if not audit['path_audit_passed'] or not audit['ownership_classification_passed']:
            raise SystemExit('Source allowlist/origin classification failed.')
        source.mkdir(parents=True, mode=0o700)
        for name in audit['candidate_paths']:
            src, dst = ROOT / name, source / name
            if src.is_symlink() or not src.is_file():
                raise RuntimeError('Non-regular source candidate: ' + name)
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dst)
        (stage / 'source-path-audit.json').write_text(json.dumps(audit, indent=2) + '\n')
        print(source)
        return
    if not a.binaries or not a.out or not (source / 'vendor/cargo').is_dir():
        raise SystemExit('pack needs --binaries, --out and cargo vendor output in staged vendor/cargo.')
    if not (source / '.cargo/config.toml').is_file():
        raise SystemExit('Missing offline source configuration.')
    output = a.out.resolve()
    if not output.is_relative_to(ROOT / '.runtime'):
        raise SystemExit('Artifact output must be inside ignored .runtime.')
    output.mkdir(parents=True, exist_ok=True)
    bundle = stage / ('glimlink-' + version + '-linux-x86_64')
    if bundle.exists():
        raise SystemExit('Bundle already exists; use a new staging path.')
    (bundle / 'bin').mkdir(parents=True)
    for name in ['agent-runtime', 'agentctl']:
        shutil.copy2(a.binaries / name, bundle / 'bin' / name)
        (bundle / 'bin' / name).chmod(0o755)
    share = bundle / 'share/glimlink'
    (share / 'config').mkdir(parents=True)
    for name in ['profile-codex.json', 'profile-antigravity.json', 'browsing.json', 'persona.txt', 'role.txt']:
        shutil.copy2(source / 'scripts/release' / name, share / 'config' / name)
    shutil.copy2(source / 'scripts/release/glimlink.service', share / 'glimlink.service')
    for origin, name in [('scripts/release/install.sh','install.sh'), ('scripts/install-engines.sh','install-engines.sh'), ('docs/deployment.md','DEPLOYMENT.md')]:
        shutil.copy2(source / origin, bundle / name)
    shutil.copy2(source / 'docs/engine-login.md', bundle / 'engine-login.md')
    for name in ['install.sh', 'install-engines.sh']:
        (bundle / name).chmod(0o755)
    shutil.copy2(source / 'LICENSE', bundle / 'LICENSE')
    shutil.copy2(source / 'NOTICE', bundle / 'NOTICE')
    shutil.copy2(source / 'crates/runtime-media/vendor/NIHAV-COPYING', bundle / 'COPYING.AGPL-3.0')
    shutil.copy2(source / 'crates/platform-qq/LICENSE-TEA-GPL-3.0', bundle / 'COPYING.GPL-3.0')
    licenses = share / 'licenses'
    for base in [source / 'crates', source / 'vendor/cargo']:
        for path in sorted(base.rglob('*')):
            if path.is_file() and path.name.upper().startswith(('LICENSE', 'COPYING', 'NOTICE', 'COPYRIGHT', 'NIHAV-COPYING')):
                dest = licenses / path.relative_to(source)
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, dest)
    for name in ['source-licenses.json', 'dependency-licenses.json', 'OPEN_SOURCE.md']:
        shutil.copy2(source / 'docs' / name, share / name)
    commit = subprocess.check_output(['git','rev-parse','HEAD'], cwd=ROOT, text=True).strip()
    manifest = {'version':version,'target':'x86_64-unknown-linux-gnu','build_baseline':'Debian 12 / Rust 1.90',
                'git_commit':commit,'git_dirty':bool(subprocess.check_output(['git','status','--porcelain'],cwd=ROOT,text=True).strip()),
                'features':[],'native_engines_bundled':False,'account_data_bundled':False,'models_bundled':False,
                'binaries':{n:sha(bundle/'bin'/n)for n in ['agent-runtime','agentctl']},
                'source_artifact':'glimlink-'+version+'-source.tar.gz',
                'license_scope':'Original code MIT; third-party notices retained. Combined Runtime includes AGPL/GPL code; not an MIT-only executable.'}
    (bundle / 'BUILD.json').write_text(json.dumps(manifest, indent=2) + '\n')
    (bundle / 'DISTRIBUTION.txt').write_text('Project-authored code remains MIT. The combined Runtime contains AGPL-3.0/GPL-3.0 components. Retained notices and license texts are in share/glimlink/licenses.\nCorresponding source, including locked registry dependencies, is the adjacent '+manifest['source_artifact']+' release asset. Build it offline with Cargo 1.90 or later.\nNative Codex/Antigravity programs, models and signers are installed separately from their providers and are not redistributed here.\n')
    artifacts = [output / (bundle.name+'.tar.gz'), output / manifest['source_artifact']]
    archive(bundle, artifacts[0])
    archive(source, artifacts[1])
    (output / 'SHA256SUMS').write_text(''.join(sha(f)+'  '+f.name+'\n'for f in artifacts))
    print(json.dumps({'artifacts':[str(f)for f in artifacts],'manifest':manifest},indent=2))

if __name__ == '__main__':
    main()
