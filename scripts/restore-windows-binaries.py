"""Reuse candidate executables only when their application sources are unchanged."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
NAMES = ('atlas', 'atlas-search-service', 'atlas-cloud-extension', 'atlas-icon-extension')


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True, encoding='utf-8')


def restore(run_id, directory):
    if not run_id.isdecimal():
        raise ValueError('The source run ID must be numeric')
    repo = os.environ['GITHUB_REPOSITORY']
    version, edition = os.environ['ATLAS_VERSION'], os.environ['ATLAS_EDITION']
    run = json.loads(command('gh', 'api', f'repos/{repo}/actions/runs/{run_id}'))
    if (run['event'] != 'workflow_dispatch' or run['head_branch'] != 'main'
            or run['head_repository']['full_name'].lower() != repo.lower()
            or run['path'] != '.github/workflows/packages.yml'):
        raise ValueError('Only manually built candidates from this repository main branch can be reused')
    revision = run['head_sha']
    if not re.fullmatch(r'[0-9a-f]{40}', revision):
        raise ValueError('Invalid source revision')
    command('git', 'fetch', '--no-tags', '--depth=1', 'origin', revision)
    changes = command('git', 'diff', '--name-only', revision, 'HEAD').splitlines()
    for path in changes:
        if path.startswith(('.github/', 'docs/', 'packaging/linux/', 'crates/atlas-search-service/src/')):
            continue
        if path.startswith('scripts/') and Path(path).suffix in ('.py', '.ps1', '.sh'):
            continue
        raise ValueError(f'Application input changed; a full build is required: {path}')

    jobs = json.loads(command('gh', 'api', f'repos/{repo}/actions/runs/{run_id}/jobs'))['jobs']
    build = next(job for job in jobs if 'EXE installer and MSIX' in job['name'])
    step = next(step for step in build['steps'] if step['name'] == 'Build optimized binaries once')
    if step['conclusion'] != 'success':
        raise ValueError('The source candidate did not finish compiling')
    # Capture trusted runner logs for parsing; never emit their terminal codes.
    log = command('gh', 'api', f'repos/{repo}/actions/jobs/{build["id"]}/logs', '--allow-escape-sequences')
    for key, value in [('ATLAS_VERSION', version), ('ATLAS_EDITION', edition)]:
        recorded = set(re.findall(r'Z\s+' + key + r': ([^\r\n]+)', log))
        if recorded != {value}:
            raise ValueError(f'The source candidate uses a different {key}: {recorded}')

    manifests = list(directory.rglob('manifest.json'))
    if len(manifests) != 1:
        raise ValueError('Expected exactly one binary/symbol manifest')
    manifest = json.loads(manifests[0].read_text(encoding='utf-8-sig'))
    if manifest['version'] != f'{version}-installer':
        raise ValueError('Symbol manifest version does not match the candidate')
    entries = {entry['exe']: entry for entry in manifest['binaries']}
    if set(entries) != {name + '.exe' for name in NAMES}:
        raise ValueError('The candidate must contain all four executable/symbol pairs')
    output = ROOT / 'target/release'
    output.mkdir(parents=True, exist_ok=True)
    for name in NAMES:
        entry = entries[name + '.exe']
        pdb = name.replace('-', '_') + '.pdb'
        if entry['pdb'] != pdb:
            raise ValueError('Unexpected PDB filename')
        executable = manifests[0].parent / entry['exe']
        if hashlib.sha256(executable.read_bytes()).hexdigest() != entry['sha256']:
            raise ValueError(f'Executable checksum mismatch: {name}')
        for filename in (entry['exe'], pdb):
            shutil.copy2(manifests[0].parent / filename, output / filename)
    provenance = dict(reusedFromRun=f'https://github.com/{repo}/actions/runs/{run_id}',
                      sourceRevision=revision, packagingRevision=os.environ['GITHUB_SHA'],
                      version=version, edition=edition,
                      rebuiltFromCurrentSource=['atlas-search-service'])
    (ROOT / 'target/reused-binaries.json').write_text(json.dumps(provenance, indent=2) + '\n', encoding='utf-8')
    print('Verified unchanged application sources, edition, version and all binary hashes; rebuilding the search service.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('run_id')
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    restore(args.run_id, args.directory.resolve())
