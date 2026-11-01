#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Print source-release candidate paths; never archive, copy, publish or read file bodies.
The only content read is this project's checked-in source/license policy JSON.
This path audit is not a secret-value scan or a legal clearance certificate.
"""
import argparse
import collections
import fnmatch
import json
import os
from pathlib import Path

PRIVATE_PARTS = {'.git', '.runtime', 'experiments', '.state', '.cache', 'target', 'reports', 'logs', 'node_modules', '__pycache__', '.venv'}
NATIVE_OR_MODEL = {'.node', '.so', '.dylib', '.dll', '.exe', '.a', '.o', '.deb', '.rpm', '.gguf', '.safetensors', '.onnx', '.p12', '.pfx', '.pem', '.key'}
TEXT_SUFFIXES = {'.rs', '.toml', '.lock', '.md', '.json', '.go', '.py', '.sh', '.proto', '.txt', '.h', '.c', '.yml', '.yaml', '.html', '.css', '.js', '.svg', '.service'}
FIXTURE_SUFFIXES = {'.pcm', '.silk', '.amr', '.bin', '.bytes', '.mp4', '.h264', '.wav', '.png', '.jpg', '.jpeg', '.gif', '.webp', '.ico'}
SENSITIVE_NAMES = ['.env', '.env.*', 'auth.json', 'credentials.json', 'cookies.txt', 'cookies.json', '*cookie*.txt', '*cookie*.json', '*credential*.json', '*password*.json', '*password*.txt', '*session*.json', 'token.json', 'tokens.json', '*-token.json', '*_token.json', '*oauth*token*', '*bduss*', 'qrcode.png', 'qrcode.json']

def rejection(path, quarantine, approved=frozenset(), excluded=frozenset()):
    if path.as_posix() in excluded: return 'excluded_from_public_source'
    if path.parts[:2] == ('operator-desktop', 'gen'): return 'generated_desktop_build_output'
    if any(p in PRIVATE_PARTS for p in path.parts): return 'private_or_build_directory'
    if path.name == 'VALIDATION.md': return 'historical_private_validation'
    name = path.name.lower()
    if any(fnmatch.fnmatchcase(name, p) for p in SENSITIVE_NAMES): return 'credential_or_login_filename'
    if path.suffix.lower() in NATIVE_OR_MODEL or '.so.' in name: return 'native_binary_model_or_key'
    if path.as_posix() in quarantine: return 'unreviewed_upstream_vector'
    if path.suffix.lower() in FIXTURE_SUFFIXES:
        if path.as_posix() in approved: return None
        parts = path.parts
        if 'tests' in parts or 'testdata' in parts or 'data' in parts: return None
        return 'unreviewed_binary_resource'
    if path.suffix.lower() in TEXT_SUFFIXES: return None
    if name in {'.gitignore', '.dockerignore', 'cargo.lock', 'license', 'notice', 'authors', 'makefile'}: return None
    if any(marker in name for marker in ['license-', 'notice-', 'copying']) or name.startswith('dockerfile'): return None
    return 'unknown_source_file_type'

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--list', action='store_true', help='Print candidate relative paths only.')
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    root = args.root.resolve()
    policy = json.loads((root / 'docs/source-licenses.json').read_text())
    release = policy['source_release']
    # The policy may narrow/extend named source roots, never escape this checkout
    # or promote private runtime roots. File entries must be direct root names.
    if any(len(Path(name).parts) != 1 or name in PRIVATE_PARTS or name in {'.', '..'}
           for name in release['root_files'] + release['directories']):
        raise SystemExit('invalid source-release allowlist root')
    if any(Path(name).is_absolute() or '..' in Path(name).parts for name in release['excluded_upstream_vector_paths']):
        raise SystemExit('invalid quarantine path')
    quarantine = set(release['excluded_upstream_vector_paths'])
    excluded = set(release.get('excluded_source_paths', []))
    if any(Path(name).is_absolute() or '..' in Path(name).parts for name in excluded):
        raise SystemExit('invalid excluded source path')
    approved = {entry['path'] for entry in release.get('approved_binary_assets', [])}
    if any(Path(name).is_absolute() or '..' in Path(name).parts for name in approved):
        raise SystemExit('invalid approved source asset path')
    candidates = []
    rejected = collections.Counter()
    symlinks = []
    for name in release['root_files']:
        path = root / name
        if path.is_symlink(): symlinks.append(name)
        elif path.is_file(): candidates.append(name)
    for name in release['directories']:
        top = root / name
        if top.is_symlink(): symlinks.append(name); continue
        if not top.is_dir(): continue
        for directory, dirs, files in os.walk(top, followlinks=False):
            keep = []
            for item in dirs:
                path = Path(directory) / item
                if path.is_symlink(): symlinks.append(path.relative_to(root).as_posix())
                elif item in PRIVATE_PARTS: rejected['private_or_build_directory'] += 1
                else: keep.append(item)
            dirs[:] = sorted(keep)
            for item in files:
                path = Path(directory) / item
                relative = path.relative_to(root)
                if path.is_symlink(): symlinks.append(relative.as_posix()); continue
                if not path.is_file(): rejected['non_regular_entry'] += 1; continue
                reason = rejection(relative, quarantine, approved, excluded)
                if reason: rejected[reason] += 1
                else: candidates.append(relative.as_posix())
    candidates = sorted(set(candidates))
    violations = [p for p in candidates if rejection(Path(p), quarantine, approved, excluded)]
    # Rules are ordered: narrow third-party/derived exceptions follow broad
    # original-source groups. Never infer ownership from a crate's license field.
    inventory = []
    for path in candidates:
        matches = [rule for rule in policy['rules']
                   if any(fnmatch.fnmatchcase(path, pattern) for pattern in rule['paths'])]
        rule = matches[-1] if matches else None
        inventory.append(dict(path=path,
                              classification=rule['classification'] if rule else 'unclassified',
                              license=rule['license'] if rule else None))
    unclassified = [item['path'] for item in inventory if item['classification'] == 'unclassified']
    ownership_counts = collections.Counter(item['classification'] for item in inventory)
    # Only policy path literals are considered, never excluded file bodies.
    referenced_quarantine = [u for u in policy.get('amr_parent_include_sites', []) if u['fixture'] in quarantine and (root/u['fixture']).is_file()]
    result = dict(schema=1, mode='dry_run_paths_only', root=str(root), candidate_count=len(candidates), candidate_paths=candidates,
                  excluded_top_level_paths=release['excluded_roots'], excluded_reason_counts=dict(sorted(rejected.items())),
                  rejected_symlink_paths=sorted(symlinks), path_audit_passed=not violations and not symlinks,
                  policy_violations=violations, quarantined_include_sites=referenced_quarantine,
                  ownership_classification_passed=not unclassified,
                  ownership_counts=dict(sorted(ownership_counts.items())),
                  unclassified_paths=unclassified, source_inventory=inventory,
                  release_blockers=policy.get('release_blockers', []),
                  publication_ready=False, reads_private_file_contents=False, creates_archive=False, publishes=False,
                  scope='Path allowlist only. Origin reviews, required fixture replacement and final public-source content review remain separate.')
    if args.list: print('\n'.join(candidates))
    else: print(json.dumps(result, ensure_ascii=False, indent=2))
    return 1 if violations or symlinks or unclassified else 0

if __name__ == '__main__': raise SystemExit(main())
