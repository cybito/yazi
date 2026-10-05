#!/usr/bin/env python3
"""Yazi install artifacts; public interface: check, pack, publish, verify."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib

PROJECT = 'yazi'
SOURCE = 'https://github.com/cybito/yazi.git'
PACKAGE = 'git.cybit.top/cybit/ias-yazi'
TYPE = 'application/vnd.cybito.install-package.v1'
MEDIA = {'release.json': 'application/json', 'SHA256SUMS': 'text/plain'}
TAG = re.compile(r'v([0-9]+\.[0-9]+\.[0-9]+)-custom\.([1-9][0-9]*)\Z')
SHA = re.compile(r'[0-9a-f]{40}\Z')
ROOT = Path(__file__).resolve().parents[2]


def run(*args, cwd=None):
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def absolute(value):
    path = Path(value)
    if not path.is_absolute():
        raise ValueError('directory must be absolute')
    return path


def identity(tag, commit, platform):
    if not TAG.fullmatch(tag) or not SHA.fullmatch(commit) or platform not in ('darwin', 'linux'):
        raise ValueError('invalid release identity')
    return f'{PACKAGE}:{tag}-{platform}-arm64'


def oras(*args, config=None, cwd=None):
    flag = '--to-registry-config' if args[0] == 'cp' else '--registry-config'
    return run('oras', *args, flag, str(config or os.environ.get('ORAS_REGISTRY_CONFIG', '/dev/null')), cwd=cwd)


def manifest_bytes(reference, config=None):
    # Preserve exact bytes: manifest digest covers serialization, not parsed JSON.
    return subprocess.check_output(['oras', 'manifest', 'fetch', reference, '--registry-config', str(config or os.environ.get('ORAS_REGISTRY_CONFIG', '/dev/null'))])


def verify(reference, output, config=None):
    if not re.fullmatch(re.escape(PACKAGE) + r'@sha256:[0-9a-f]{64}', reference):
        raise ValueError('verification requires this package immutable digest')
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        raise ValueError('verification directory must be empty')
    raw = manifest_bytes(reference, config)
    if digest(raw) != reference.rsplit(':', 1)[1]:
        raise ValueError('manifest digest mismatch')
    manifest = json.loads(raw)
    if manifest.get('artifactType') != TYPE:
        raise ValueError('wrong artifact type')
    descriptors = manifest.get('layers', [])
    names = []
    for layer in descriptors:
        name = layer.get('annotations', {}).get('org.opencontainers.image.title', '')
        if not name or Path(name).name != name or name in names:
            raise ValueError('unsafe or duplicate layer filename')
        names.append(name)
    oras('pull', reference, '--output', str(output), config=config)
    for layer, name in zip(descriptors, names):
        path = output / name
        if path.is_symlink() or not path.is_file():
            raise ValueError('missing regular layer')
        data = path.read_bytes()
        if len(data) != layer['size'] or 'sha256:' + digest(data) != layer['digest']:
            raise ValueError('layer mismatch')
        expected = MEDIA.get(name, 'application/gzip' if name.endswith('.tar.gz') else None)
        if expected is None or layer['mediaType'] != expected:
            raise ValueError('unexpected layer media type')
    receipt = json.loads((output / 'release.json').read_bytes())
    fields = {'schema', 'project', 'source_repo', 'source_commit', 'release_tag', 'platform', 'architecture', 'toolchains', 'files'}
    if set(receipt) != fields or receipt['schema'] != 1 or receipt['project'] != PROJECT or receipt['source_repo'] != SOURCE or receipt['architecture'] != 'arm64':
        raise ValueError('invalid receipt')
    identity(receipt['release_tag'], receipt['source_commit'], receipt['platform'])
    if not isinstance(receipt['toolchains'], dict) or not receipt['toolchains'] or not all(isinstance(v, str) for v in receipt['toolchains'].values()):
        raise ValueError('invalid toolchains')
    files = receipt['files']
    expected_name = f"yazi-{receipt['release_tag']}-{receipt['platform']}-arm64.tar.gz"
    config_descriptor = manifest['config']
    config_bytes = subprocess.check_output(['oras', 'blob', 'fetch', '--output', '-', PACKAGE + '@' + config_descriptor['digest'], '--registry-config', str(config or os.environ.get('ORAS_REGISTRY_CONFIG', '/dev/null'))])
    if len(config_bytes) != config_descriptor['size'] or 'sha256:' + digest(config_bytes) != config_descriptor['digest']:
        raise ValueError('config descriptor mismatch')
    if len(files) != 1 or set(files[0]) != {'name', 'sha256', 'size'} or files[0]['name'] != expected_name:
        raise ValueError('invalid payload list')
    if set(names) != {'release.json', 'SHA256SUMS', expected_name} or set(p.name for p in output.iterdir()) != set(names):
        raise ValueError('unexpected payload files')
    for file in files:
        data = (output / file['name']).read_bytes()
        if digest(data) != file['sha256'] or len(data) != file['size']:
            raise ValueError('receipt hash mismatch')
    checks = ''.join(f'{digest((output / n).read_bytes())}  {n}\n' for n in sorted([expected_name, 'release.json']))
    if (output / 'SHA256SUMS').read_text() != checks:
        raise ValueError('checksum file mismatch')
    annotations = manifest.get('annotations', {})
    for key, value in {'source': SOURCE, 'revision': receipt['source_commit'], 'version': receipt['release_tag']}.items():
        if annotations.get('org.opencontainers.image.' + key) != value:
            raise ValueError('manifest identity mismatch')
    return receipt


def check(tag, commit, platform, output, config=None):
    reference = identity(tag, commit, platform)
    result = subprocess.run(['oras', 'manifest', 'fetch', '--descriptor', reference, '--registry-config', str(config or os.environ.get('ORAS_REGISTRY_CONFIG', '/dev/null'))], capture_output=True, text=True)
    if result.returncode:
        if re.search(r'\b(?:MANIFEST_UNKNOWN|NAME_UNKNOWN|manifest_unknown|name_unknown)\b', result.stderr):
            return {'exists': False}
        raise RuntimeError(result.stderr)
    descriptor = json.loads(result.stdout)
    immutable = PACKAGE + '@' + descriptor['digest']
    receipt = verify(immutable, output, config)
    if (receipt['source_commit'], receipt['release_tag'], receipt['platform']) != (commit, tag, platform):
        raise ValueError('existing release identity differs; refusing overwrite')
    return {'exists': True, 'reference': immutable}


INSTALL = '''#!/bin/sh
set -eu
prefix=${HOME:?}/.local
if [ "$#" -gt 0 ]; then
  [ "$#" -eq 2 ] && [ "$1" = --prefix ] || { echo 'usage: install.sh [--prefix ABSOLUTE]' >&2; exit 2; }
  prefix=$2
fi
case "$prefix" in /*) ;; *) echo 'prefix must be absolute' >&2; exit 2;; esac
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# Preflight the entire tree before copying; refuse different files and symlinks.
for tree in bin share; do
  [ ! -L "$prefix/$tree" ] || exit 1
  find "$root/$tree" -type f | while IFS= read -r src; do
    rel=${src#"$root/"}; dst=$prefix/$rel
    parent=$(dirname "$dst")
    while [ "$parent" != / ]; do [ ! -L "$parent" ] || exit 1; parent=$(dirname "$parent"); done
    if [ -e "$dst" ] || [ -L "$dst" ]; then
      [ -f "$dst" ] && [ ! -L "$dst" ] && cmp -s "$src" "$dst" || { echo "refusing overwrite: $dst" >&2; exit 1; }
    fi
  done
 done
for tree in bin share; do
  find "$root/$tree" -type f | while IFS= read -r src; do
    rel=${src#"$root/"}; dst=$prefix/$rel
    if [ ! -e "$dst" ]; then mkdir -p "$(dirname "$dst")"; cp -p "$src" "$dst"; fi
  done
 done
'''


def pack(args):
    identity(args.tag, args.commit, args.platform)
    if run('git', 'rev-parse', 'HEAD', cwd=ROOT) != args.commit:
        raise ValueError('source differs from checkout')
    source, output = absolute(args.input_dir), absolute(args.output_dir)
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        raise ValueError('pack output must be empty')
    with tempfile.TemporaryDirectory() as temp:
        tree = Path(temp) / 'yazi'
        shutil.copytree(source / 'bin', tree / 'bin')
        shutil.copytree(source / 'share', tree / 'share')
        for name in ('LICENSE', 'LICENSE-ICONS', 'README.md'):
            shutil.copy2(ROOT / name, tree / name)
        for path in ROOT.rglob('NOTICE*'):
            if '.git' not in path.parts and 'target' not in path.parts and path.is_file():
                destination = tree / 'share/doc/yazi/notices' / path.relative_to(ROOT)
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, destination)
        (tree / 'install.sh').write_text(INSTALL)
        (tree / 'install.sh').chmod(0o755)
        name = f'yazi-{args.tag}-{args.platform}-arm64.tar.gz'
        with tarfile.open(output / name, 'w:gz') as archive:
            archive.add(tree, arcname='yazi')
    data = (output / name).read_bytes()
    receipt = dict(schema=1, project=PROJECT, source_repo=SOURCE, source_commit=args.commit, release_tag=args.tag, platform=args.platform, architecture='arm64', toolchains=json.loads((source / 'toolchains.json').read_text()), files=[dict(name=name, sha256=digest(data), size=len(data))])
    (output / 'release.json').write_text(json.dumps(receipt, sort_keys=True, indent=2) + '\n')
    (output / 'SHA256SUMS').write_text(''.join(f'{digest((output / n).read_bytes())}  {n}\n' for n in sorted([name, 'release.json'])))
    # Exercise the shipped installer twice, never the real HOME.
    with tempfile.TemporaryDirectory() as temp:
        subprocess.run(['tar', '-xzf', str(output / name), '-C', temp], check=True)
        prefix = str(Path(temp) / 'prefix')
        for _ in range(2):
            subprocess.run(['sh', str(Path(temp) / 'yazi/install.sh'), '--prefix', prefix], check=True)
        for binary in ('yazi', 'ya'):
            subprocess.run([prefix + '/bin/' + binary, '--version'], check=True, stdout=sys.stderr)
    return {'directory': str(output)}


def publish(args):
    directory, config = absolute(args.directory), absolute(args.registry_config)
    receipt = json.loads((directory / 'release.json').read_text())
    reference = identity(receipt['release_tag'], receipt['source_commit'], receipt['platform'])
    with tempfile.TemporaryDirectory() as temp:
        existing = check(receipt['release_tag'], receipt['source_commit'], receipt['platform'], Path(temp) / 'existing', config)
        if existing['exists']:
            for name in [f['name'] for f in receipt['files']] + ['release.json', 'SHA256SUMS']:
                if (directory / name).read_bytes() != (Path(temp) / 'existing' / name).read_bytes():
                    raise ValueError('existing tag bytes differ')
            return dict(reference=existing['reference'], digest=existing['reference'].split('@')[1])
        layout = str(Path(temp) / 'layout')
        local = layout + ':release'
        from datetime import datetime, timezone
        timestamp = int(run('git', 'show', '-s', '--format=%ct', receipt['source_commit'], cwd=ROOT))
        created = datetime.fromtimestamp(timestamp, timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
        args_list = ['push', '--oci-layout', local, '--artifact-type', TYPE]
        for key, value in {'source': SOURCE, 'revision': receipt['source_commit'], 'version': receipt['release_tag'], 'created': created}.items():
            args_list += ['--annotation', f'org.opencontainers.image.{key}={value}']
        for name in [f['name'] for f in receipt['files']] + ['release.json', 'SHA256SUMS']:
            args_list.append(name + ':' + MEDIA.get(name, 'application/gzip'))
        oras(*args_list, config=config, cwd=directory)
        index = json.loads((Path(layout) / 'index.json').read_text())
        dgst = index['manifests'][0]['digest']
        immutable = PACKAGE + '@' + dgst
        oras('cp', '--from-oci-layout', local, reference, config=config)
        verified = verify(immutable, Path(temp) / 'pulled', config)
        if verified != receipt:
            raise ValueError('published receipt differs')
        if json.loads(oras('manifest', 'fetch', '--descriptor', reference, config=config))['digest'] != dgst:
            raise ValueError('published tag digest differs')
        return dict(reference=immutable, digest=dgst)


def validate_event():
    event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
    release = event['release']
    tag = release['tag_name']
    if os.environ['GITHUB_REPOSITORY'] != 'cybito/yazi' or release['draft'] or not TAG.fullmatch(tag) or release['assets']:
        raise ValueError('invalid release event or nonempty assets')
    commit = run('git', 'rev-parse', '--verify', 'refs/tags/' + tag + '^{commit}', cwd=ROOT)
    if run('git', 'rev-parse', 'HEAD', cwd=ROOT) != commit:
        raise ValueError('validation checkout differs from dereferenced release tag')
    identity(tag, commit, 'linux')
    subprocess.run(['git', 'merge-base', '--is-ancestor', commit, 'refs/remotes/origin/custom'], cwd=ROOT, check=True)
    for path in ('.github/workflows/custom-release.yml', '.github/scripts/custom-release.sh', '.github/scripts/package-release.py'):
        run('git', 'cat-file', '-e', commit + ':' + path, cwd=ROOT)
    declaration = tomllib.loads(run('git', 'show', commit + ':Cargo.toml', cwd=ROOT))['workspace']['package']['version']
    if TAG.fullmatch(tag).group(1) != declaration:
        raise ValueError('release base differs from workspace version')
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        output.write(f'commit={commit}\ntag={tag}\nversion={declaration}\n')
    return dict(commit=commit, tag=tag)


def summarize():
    event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
    release = event['release']
    if release['assets']:
        raise ValueError('GitHub assets must remain empty')
    tag, commit = os.environ['RELEASE_TAG'], os.environ['SOURCE_SHA']
    references = []
    with tempfile.TemporaryDirectory() as temp:
        for platform in ('darwin', 'linux'):
            checked = check(tag, commit, platform, Path(temp) / platform)
            if not checked['exists']:
                raise ValueError('missing platform')
            references.append((platform, checked['reference']))
    start, end = '<!-- custom-builds:start -->', '<!-- custom-builds:end -->'
    block = start + f'\nSource: `{SOURCE}` at `{commit}`; release `{tag}`.\n\n[Forgejo package](https://git.cybit.top/cybit/-/packages/container/ias-yazi)\n\n' + '\n'.join(f'### {p} ARM64\n```sh\noras pull {r}\n```\n' for p, r in references) + end
    current = json.loads(run('gh', 'api', f'repos/cybito/yazi/releases/{release["id"]}'))
    if current['assets']:
        raise ValueError('GitHub assets must remain empty')
    body = current.get('body') or ''
    if start in body or end in body:
        if body.count(start) != 1 or body.count(end) != 1 or body.index(start) > body.index(end):
            raise ValueError('malformed managed notes block')
        body = body[:body.index(start)] + block + body[body.index(end) + len(end):]
    else:
        body += '\n\n' + block
    subprocess.run(['gh', 'api', '--method', 'PATCH', f'repos/cybito/yazi/releases/{release["id"]}', '--input', '-'], input=json.dumps({'body': body}), text=True, check=True, stdout=subprocess.DEVNULL)
    return {'references': dict(references)}


def regression_tests():
    """Real git ancestry and malicious event regression fixtures."""
    global ROOT
    from unittest.mock import patch
    original = ROOT
    with tempfile.TemporaryDirectory() as temp:
        ROOT = Path(temp)
        subprocess.run(['git', 'init', '-q', str(ROOT)], check=True)
        run('git', 'config', 'user.email', 'fixture@example.invalid', cwd=ROOT)
        run('git', 'config', 'user.name', 'fixture', cwd=ROOT)
        for path in ('.github/workflows/custom-release.yml', '.github/scripts/custom-release.sh', '.github/scripts/package-release.py'):
            destination = ROOT / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text('fixture\n')
        (ROOT / 'Cargo.toml').write_text('[workspace.package]\nversion="26.9.1"\n')
        run('git', 'add', '.', cwd=ROOT)
        run('git', 'commit', '-qm', 'custom fixture', cwd=ROOT)
        commit = run('git', 'rev-parse', 'HEAD', cwd=ROOT)
        run('git', 'update-ref', 'refs/remotes/origin/custom', commit, cwd=ROOT)
        run('git', 'tag', 'v26.9.1-custom.1', cwd=ROOT)
        event = ROOT / 'event.json'
        def write_event(tag):
            event.write_text(json.dumps({'release': {'tag_name': tag, 'draft': False, 'assets': []}}))
        with patch.dict(os.environ, GITHUB_EVENT_PATH=str(event), GITHUB_REPOSITORY='cybito/yazi', GITHUB_OUTPUT=str(ROOT / 'output')):
            write_event('v26.9.1-custom.1')
            assert validate_event()['commit'] == commit
            run('git', 'checkout', '--orphan', 'upstream-only', cwd=ROOT)
            run('git', 'commit', '-qm', 'unrelated upstream', cwd=ROOT)
            run('git', 'tag', 'v26.9.1-custom.2', cwd=ROOT)
            for tag in ('v26.9.1-custom.2', 'v26.9.1', 'v26.9.1-custom.1;touch PWNED'):
                write_event(tag)
                try:
                    validate_event()
                except (ValueError, subprocess.CalledProcessError):
                    pass
                else:
                    raise AssertionError('invalid event accepted')
            assert not (ROOT / 'PWNED').exists()
        for message in ('unauthorized: authentication required', 'connection timed out'):
            with patch('subprocess.run', return_value=subprocess.CompletedProcess([], 1, '', message)):
                try:
                    check('v26.9.1-custom.1', commit, 'linux', ROOT / 'missing')
                except RuntimeError:
                    pass
                else:
                    raise AssertionError('registry error treated as missing')
        with patch('subprocess.run', return_value=subprocess.CompletedProcess([], 1, '', 'MANIFEST_UNKNOWN')):
            assert check('v26.9.1-custom.1', commit, 'linux', ROOT / 'missing') == {'exists': False}
        with patch('subprocess.run', return_value=subprocess.CompletedProcess([], 0, json.dumps({'digest': 'sha256:' + 'a' * 64}), '')), patch(__name__ + '.verify', return_value={'source_commit': 'b' * 40, 'release_tag': 'v26.9.1-custom.1', 'platform': 'linux'}):
            try:
                check('v26.9.1-custom.1', commit, 'linux', ROOT / 'wrong')
            except ValueError:
                pass
            else:
                raise AssertionError('wrong receipt reused')
    ROOT = original
    return {'regressions': 'passed'}


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest='command', required=True)
    for command in ('check', 'pack'):
        p = sub.add_parser(command)
        for arg in ('tag', 'commit', 'platform', 'output-dir'):
            p.add_argument('--' + arg, required=True)
        if command == 'pack':
            p.add_argument('--input-dir', required=True)
    p = sub.add_parser('verify'); p.add_argument('--reference', required=True); p.add_argument('--output-dir', required=True)
    p = sub.add_parser('publish'); p.add_argument('--directory', required=True); p.add_argument('--registry-config', required=True)
    sub.add_parser('validate-event'); sub.add_parser('summarize'); sub.add_parser('self-test')
    args = parser.parse_args()
    if args.command == 'check': result = check(args.tag, args.commit, args.platform, absolute(args.output_dir))
    elif args.command == 'verify': result = verify(args.reference, absolute(args.output_dir))
    elif args.command == 'pack': result = pack(args)
    elif args.command == 'publish': result = publish(args)
    elif args.command == 'validate-event': result = validate_event()
    elif args.command == 'self-test': result = regression_tests()
    else: result = summarize()
    print(json.dumps(result))

if __name__ == '__main__':
    try:
        main()
    except (ValueError, RuntimeError, OSError, subprocess.CalledProcessError, KeyError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
