#!/usr/bin/env python3
"""Yazi install artifacts and immutable GitHub Release assets."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import filecmp
import subprocess
import sys
import tarfile
import tempfile
import tomllib

PROJECT = 'yazi'
SOURCE = 'https://github.com/cybito/yazi.git'
TAG = re.compile(r'v([0-9]+\.[0-9]+\.[0-9]+)-custom\.([1-9][0-9]*)\Z')
SHA = re.compile(r'[0-9a-f]{40}\Z')
ROOT = Path(__file__).resolve().parents[2]
MAX_FILE = 2 * 1024**3
MAX_ASSETS = 1000


def run(*args, cwd=None):
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def digest(data):
    return hashlib.sha256(data).hexdigest()

def file_digest(path):
    with open(path, 'rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def absolute(value):
    path = Path(value)
    if not path.is_absolute():
        raise ValueError('directory must be absolute')
    return path


def identity(tag, commit, platform):
    if not TAG.fullmatch(tag) or not SHA.fullmatch(commit) or platform not in ('darwin', 'linux'):
        raise ValueError('invalid release identity')
    return tag, platform


def package_names(tag, platform):
    package = f'yazi-{tag}-{platform}-arm64.tar.gz'
    return [package, 'release.json', 'SHA256SUMS']


def gh_assets(tag):
    result = json.loads(run('gh', 'release', 'view', tag, '--repo', 'cybito/yazi', '--json', 'assets'))
    return {asset['name']: asset for asset in result['assets']}

def download_asset(tag, name, destination):
    subprocess.run(
        ['gh', 'release', 'download', tag, '--repo', 'cybito/yazi', '--pattern', name, '--dir', str(destination)],
        check=True,
    )


def asset_name(tag, platform, filename):
    return f'{tag}-{platform}-{filename}'


def verify_directory(directory, tag=None, commit=None, platform=None):
    directory = Path(directory)
    receipt = json.loads((directory / 'release.json').read_text())
    fields = {'schema', 'project', 'source_repo', 'source_commit', 'release_tag', 'platform', 'architecture', 'toolchains', 'files'}
    if set(receipt) != fields or receipt['schema'] != 1 or receipt['project'] != PROJECT or receipt['source_repo'] != SOURCE or receipt['architecture'] != 'arm64':
        raise ValueError('invalid release receipt')
    identity(receipt['release_tag'], receipt['source_commit'], receipt['platform'])
    if tag is not None and (receipt['release_tag'], receipt['source_commit'], receipt['platform']) != (tag, commit, platform):
        raise ValueError('release identity differs')
    if not isinstance(receipt['toolchains'], dict) or not receipt['toolchains'] or not all(isinstance(v, str) for v in receipt['toolchains'].values()):
        raise ValueError('invalid toolchains')
    expected = package_names(receipt['release_tag'], receipt['platform'])
    if set(p.name for p in directory.iterdir()) != set(expected):
        raise ValueError('unexpected asset files')
    package = expected[0]
    files = receipt['files']
    if len(files) != 1 or set(files[0]) != {'name', 'sha256', 'size'} or files[0]['name'] != package:
        raise ValueError('invalid payload list')
    for record in files:
        path = directory / record['name']
        if file_digest(path) != record['sha256'] or path.stat().st_size != record['size']:
            raise ValueError('receipt hash mismatch')
    checks = ''.join(f'{file_digest(directory / name)}  {name}\n' for name in sorted((package, 'release.json')))
    if (directory / 'SHA256SUMS').read_text() != checks:
        raise ValueError('checksum file mismatch')
    return receipt



def check(tag, commit, platform, output):
    identity(tag, commit, platform)
    assets = gh_assets(tag)
    expected = package_names(tag, platform)
    prefix = f'{tag}-{platform}-'
    expected_assets = {asset_name(tag, platform, original) for original in expected}
    unexpected = [name for name in assets if name.startswith(prefix) and name not in expected_assets]
    if unexpected:
        raise ValueError(f'unexpected colliding platform assets: {unexpected}')
    available = [asset_name(tag, platform, name) for name in expected if asset_name(tag, platform, name) in assets]
    if not available:
        return {'exists': False}
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        raise ValueError('check output directory must be empty')
    with tempfile.TemporaryDirectory() as temp:
        downloaded = Path(temp)
        for name in available:
            download_asset(tag, name, downloaded)
            original = name[len(tag + '-' + platform + '-'):]
            shutil.move(downloaded / name, output / original)
    # Partial platform sets are valid; only reuse when all package bytes exist
    # and form a coherent, receipt-verified set.
    if len(available) != len(expected):
        shutil.rmtree(output)
        return {'exists': False}
    verify_directory(output, tag, commit, platform)
    return {'exists': True, 'assets': available}


INSTALL = '''#!/bin/sh
set -eu
prefix=${HOME:?}/.local
if [ "$#" -gt 0 ]; then
  [ "$#" -eq 2 ] && [ "$1" = --prefix ] || { echo 'usage: install.sh [--prefix ABSOLUTE]' >&2; exit 2; }
  prefix=$2
fi
case "$prefix" in /*) ;; *) echo 'prefix must be absolute' >&2; exit 2;; esac
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
for tree in bin share; do
  [ ! -L "$prefix/$tree" ] || exit 1
  find "$root/$tree" -type f | while IFS= read -r src; do
    rel=${src#"$root/"}; dst=$prefix/$rel; parent=$(dirname "$dst")
    while [ "$parent" != / ]; do [ ! -L "$parent" ] || exit 1; parent=$(dirname "$parent"); done
    if [ -e "$dst" ] || [ -L "$dst" ]; then [ -f "$dst" ] && [ ! -L "$dst" ] && cmp -s "$src" "$dst" || { echo "refusing overwrite: $dst" >&2; exit 1; }; fi
  done
done
for tree in bin share; do
  find "$root/$tree" -type f | while IFS= read -r src; do rel=${src#"$root/"}; dst=$prefix/$rel; if [ ! -e "$dst" ]; then mkdir -p "$(dirname "$dst")"; cp -p "$src" "$dst"; fi; done
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
    archive = output / name
    receipt = dict(schema=1, project=PROJECT, source_repo=SOURCE, source_commit=args.commit, release_tag=args.tag, platform=args.platform, architecture='arm64', toolchains=json.loads((source / 'toolchains.json').read_text()), files=[dict(name=name, sha256=file_digest(archive), size=archive.stat().st_size)])
    (output / 'release.json').write_text(json.dumps(receipt, sort_keys=True, indent=2) + '\n')
    (output / 'SHA256SUMS').write_text(''.join(f'{file_digest(output / n)}  {n}\n' for n in sorted([name, 'release.json'])))
    with tempfile.TemporaryDirectory() as temp:
        temp_root = Path(temp).resolve()
        subprocess.run(['tar', '-xzf', str(output / name), '-C', str(temp_root)], check=True)
        prefix = str(temp_root / 'prefix')
        for _ in range(2):
            subprocess.run(['sh', str(temp_root / 'yazi/install.sh'), '--prefix', prefix], check=True)
        for binary in ('yazi', 'ya'):
            subprocess.run([prefix + '/bin/' + binary, '--version'], check=True, stdout=sys.stderr)
    return {'directory': str(output)}


def publish(args):
    directory = absolute(args.directory)
    receipt = verify_directory(directory)
    tag, platform = receipt['release_tag'], receipt['platform']
    assets = gh_assets(tag)
    expected = package_names(tag, platform)
    platform_assets = [asset_name(tag, platform, name) for name in expected]
    prefix = f'{tag}-{platform}-'
    unexpected = [name for name in assets if name.startswith(prefix) and name not in set(platform_assets)]
    if unexpected:
        raise ValueError(f'unexpected colliding platform assets: {unexpected}')
    if len(assets) > MAX_ASSETS:
        raise ValueError('release already exceeds asset limit')
    with tempfile.TemporaryDirectory() as temp:
        existing_dir = Path(temp)
        existing_names = [name for name in platform_assets if name in assets]
        for remote_name in existing_names:
            asset = assets[remote_name]
            if asset['size'] >= MAX_FILE:
                raise ValueError('existing release asset exceeds 2 GiB')
            download_asset(tag, remote_name, existing_dir)
            original = remote_name[len(tag + '-' + platform + '-'):]
            if not filecmp.cmp(existing_dir / remote_name, directory / original, shallow=False):
                raise ValueError(f'refusing to overwrite differing asset {remote_name}')
        missing = [name for name in platform_assets if name not in assets]
        if len(assets) + len(missing) > MAX_ASSETS:
            raise ValueError('upload would exceed 1000 release assets')
        for remote_name in missing:
            original = remote_name[len(prefix):]
            if (directory / original).stat().st_size >= MAX_FILE:
                raise ValueError(f'asset reaches GitHub 2 GiB limit: {remote_name}')
        if missing:
            staging = Path(temp) / 'upload'
            staging.mkdir()
            staged = []
            for remote_name in missing:
                original = remote_name[len(prefix):]
                target = staging / remote_name
                shutil.copyfile(directory / original, target)
                staged.append(str(target))
            subprocess.run(['gh', 'release', 'upload', tag, '--repo', 'cybito/yazi', *staged], check=True)
        result = check(tag, receipt['source_commit'], platform, Path(temp) / 'verified')
        if not result['exists']:
            raise ValueError('uploaded assets failed readback validation')
        return {'assets': result['assets']}


def validate_event():
    event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
    release = event['release']
    tag = release['tag_name']
    if os.environ['GITHUB_REPOSITORY'] != 'cybito/yazi' or release['draft'] or not TAG.fullmatch(tag):
        raise ValueError('invalid release event')
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
    tag, commit = os.environ['RELEASE_TAG'], os.environ['SOURCE_SHA']
    links = []
    with tempfile.TemporaryDirectory() as temp:
        for platform in ('darwin', 'linux'):
            checked = check(tag, commit, platform, Path(temp) / platform)
            if not checked['exists']:
                raise ValueError('missing or invalid platform assets')
            links.append((platform, checked['assets']))
    start, end = '<!-- custom-builds:start -->', '<!-- custom-builds:end -->'
    block = start + f'\nSource: `{SOURCE}` at `{commit}`; release `{tag}`.\n\n' + '\n'.join(f'### {platform} ARM64\n' + '\n'.join(f'- [{name}](https://github.com/cybito/yazi/releases/download/{tag}/{name})' for name in names) for platform, names in links) + '\n' + end
    current = json.loads(run('gh', 'api', f'repos/cybito/yazi/releases/{release["id"]}'))
    body = current.get('body') or ''
    if start in body or end in body:
        if body.count(start) != 1 or body.count(end) != 1 or body.index(start) > body.index(end):
            raise ValueError('malformed managed notes block')
        body = body[:body.index(start)] + block + body[body.index(end) + len(end):]
    else:
        body += '\n\n' + block
    subprocess.run(['gh', 'api', '--method', 'PATCH', f'repos/cybito/yazi/releases/{release["id"]}', '--input', '-'], input=json.dumps({'body': body}), text=True, check=True, stdout=subprocess.DEVNULL)
    return {'assets': dict(links)}


def regression_tests():
    global ROOT
    from unittest.mock import patch
    original = ROOT
    with tempfile.TemporaryDirectory() as temp:
        ROOT = Path(temp)
        subprocess.run(['git', 'init', '-q', str(ROOT)], check=True)
        run('git', 'config', 'user.email', 'fixture@example.invalid', cwd=ROOT)
        run('git', 'config', 'user.name', 'fixture', cwd=ROOT)
        for path in ('.github/workflows/custom-release.yml', '.github/scripts/custom-release.sh', '.github/scripts/package-release.py'):
            destination = ROOT / path; destination.parent.mkdir(parents=True, exist_ok=True); destination.write_text('fixture\n')
        (ROOT / 'Cargo.toml').write_text('[workspace.package]\nversion="26.9.1"\n')
        run('git', 'add', '.', cwd=ROOT); run('git', 'commit', '-qm', 'custom fixture', cwd=ROOT)
        commit = run('git', 'rev-parse', 'HEAD', cwd=ROOT); run('git', 'update-ref', 'refs/remotes/origin/custom', commit, cwd=ROOT); run('git', 'tag', 'v26.9.1-custom.1', cwd=ROOT)
        event = ROOT / 'event.json'
        def write_event(tag): event.write_text(json.dumps({'release': {'tag_name': tag, 'draft': False, 'assets': []}}))
        with patch.dict(os.environ, GITHUB_EVENT_PATH=str(event), GITHUB_REPOSITORY='cybito/yazi', GITHUB_OUTPUT=str(ROOT / 'output')):
            write_event('v26.9.1-custom.1'); assert validate_event()['commit'] == commit
            run('git', 'checkout', '--orphan', 'upstream-only', cwd=ROOT); run('git', 'commit', '-qm', 'unrelated upstream', cwd=ROOT); run('git', 'tag', 'v26.9.1-custom.2', cwd=ROOT)
            for tag in ('v26.9.1-custom.2', 'v26.9.1', 'v26.9.1-custom.1;touch PWNED'):
                write_event(tag)
                try: validate_event()
                except (ValueError, subprocess.CalledProcessError): pass
                else: raise AssertionError('invalid event accepted')
            assert not (ROOT / 'PWNED').exists()
        fixture = ROOT / 'asset'; fixture.mkdir()
        data = b'payload'; (fixture / 'yazi-v26.9.1-custom.1-linux-arm64.tar.gz').write_bytes(data)
        receipt = dict(schema=1, project=PROJECT, source_repo=SOURCE, source_commit=commit, release_tag='v26.9.1-custom.1', platform='linux', architecture='arm64', toolchains={'rustc': 'rustc 1.97.1'}, files=[dict(name='yazi-v26.9.1-custom.1-linux-arm64.tar.gz', sha256=digest(data), size=len(data))])
        (fixture / 'release.json').write_text(json.dumps(receipt)); (fixture / 'SHA256SUMS').write_text(''.join(f'{digest((fixture / n).read_bytes())}  {n}\n' for n in sorted(['release.json', receipt['files'][0]['name']])))
        assert verify_directory(fixture, receipt['release_tag'], commit, 'linux') == receipt
        (fixture / 'SHA256SUMS').write_text('bad')
        try: verify_directory(fixture)
        except ValueError: pass
        else: raise AssertionError('bad checksum accepted')
        (fixture / 'SHA256SUMS').write_text(''.join(f'{digest((fixture / n).read_bytes())}  {n}\n' for n in sorted(['release.json', receipt['files'][0]['name']])))
        assets = {asset_name(receipt['release_tag'], 'linux', n): {'size': 1} for n in package_names(receipt['release_tag'], 'linux')}
        unexpected = dict(assets)
        unexpected[asset_name(receipt['release_tag'], 'linux', 'extra.tar.gz')] = {'size': 1}
        with patch(__name__ + '.gh_assets', return_value=unexpected):
            try: check(receipt['release_tag'], commit, 'linux', ROOT / 'collision')
            except ValueError: pass
            else: raise AssertionError('unexpected platform asset accepted')
        with patch(__name__ + '.gh_assets', return_value=assets), patch(__name__ + '.download_asset', side_effect=lambda tag, name, dst: shutil.copy2(fixture / name[len(tag + '-linux-'):], dst / name)):
            result = check(receipt['release_tag'], commit, 'linux', ROOT / 'download')
            assert result['exists'] and len(result['assets']) == 3
        bad_asset = {asset_name(receipt['release_tag'], 'linux', package_names(receipt['release_tag'], 'linux')[0]): {'size': len(data) + 1}}
        with patch(__name__ + '.gh_assets', return_value=bad_asset), patch(__name__ + '.download_asset', side_effect=lambda tag, name, dst: (dst / name).write_bytes(b'different')):
            try: publish(argparse.Namespace(directory=str(fixture)))
            except ValueError: pass
            else: raise AssertionError('mismatching partial asset accepted')
        partial = dict(list(assets.items())[:1])
        with patch(__name__ + '.gh_assets', return_value=partial), patch(__name__ + '.download_asset', side_effect=lambda tag, name, dst: shutil.copy2(fixture / name[len(tag + '-linux-'):], dst / name)):
            result = check(receipt['release_tag'], commit, 'linux', ROOT / 'partial')
            assert result == {'exists': False}
        partial_assets = {asset_name(receipt['release_tag'], 'linux', package_names(receipt['release_tag'], 'linux')[0]): {'size': len(data)}}
        uploaded = []
        def upload(command, **kwargs):
            uploaded.append(command)
            for path in command[command.index('--repo') + 2:]:
                if path == '--clobber':
                    raise AssertionError('release upload must not clobber')
                if path.startswith('/'):
                    name = Path(path).name
                    partial_assets[name] = {'size': Path(path).stat().st_size}
            return subprocess.CompletedProcess(command, 0)
        with patch(__name__ + '.gh_assets', side_effect=lambda tag: dict(partial_assets)), patch(__name__ + '.download_asset', side_effect=lambda tag, name, dst: shutil.copy2(fixture / name[len(tag + '-linux-'):], dst / name)), patch('subprocess.run', side_effect=upload):
            published = publish(argparse.Namespace(directory=str(fixture)))
        assert len(published['assets']) == 3
        assert len(uploaded) == 1 and 'upload' in uploaded[0] and '--clobber' not in uploaded[0]
    ROOT = original
    return {'regressions': 'passed'}


def main():
    parser = argparse.ArgumentParser(); sub = parser.add_subparsers(dest='command', required=True)
    for command in ('check', 'pack'):
        p = sub.add_parser(command)
        for arg in ('tag', 'commit', 'platform', 'output-dir'): p.add_argument('--' + arg, required=True)
        if command == 'pack': p.add_argument('--input-dir', required=True)
    p = sub.add_parser('publish'); p.add_argument('--directory', required=True)
    sub.add_parser('validate-event'); sub.add_parser('summarize'); sub.add_parser('self-test')
    args = parser.parse_args()
    if args.command == 'check': result = check(args.tag, args.commit, args.platform, absolute(args.output_dir))
    elif args.command == 'pack': result = pack(args)
    elif args.command == 'publish': result = publish(args)
    elif args.command == 'validate-event': result = validate_event()
    elif args.command == 'self-test': result = regression_tests()
    else: result = summarize()
    print(json.dumps(result))

if __name__ == '__main__':
    try: main()
    except (ValueError, RuntimeError, OSError, subprocess.CalledProcessError, KeyError) as error:
        print(str(error), file=sys.stderr); sys.exit(1)
