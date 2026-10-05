#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == tools && $# == 1 ]]; then
  case "$(uname -s)" in Darwin) os=darwin ;; Linux) os=linux ;; *) exit 2 ;; esac
  [[ $(uname -m) == arm64 || $(uname -m) == aarch64 ]]
  tools="$RUNNER_TEMP/custom-tools"
  mkdir -p "$tools"
  archive="oras_1.3.3_${os}_arm64.tar.gz"
  url=https://github.com/oras-project/oras/releases/download/v1.3.3
  curl --fail --location --retry 3 "$url/$archive" -o "$tools/$archive"
  curl --fail --location --retry 3 "$url/oras_1.3.3_checksums.txt" -o "$tools/checksums.txt"
  python3 - "$tools" "$archive" "$os" <<'PY'
import hashlib, pathlib, sys
root, name, platform = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
lines = [line.split() for line in (root / 'checksums.txt').read_text().splitlines()]
expected = [parts[0] for parts in lines if len(parts) == 2 and parts[1].lstrip('*') == name]
pinned = {'darwin': 'f33fc12753c54172b0d0d19eaa0318d3f90fe9b094d96e8b259c881713c92e1c', 'linux': 'ac7156f93a21e903f7ad606c792f3560f17e0cd0e36365634701b1e7cc4e4eca'}[platform]
if len(expected) != 1 or expected[0] != pinned or hashlib.sha256((root / name).read_bytes()).hexdigest() != pinned:
    raise SystemExit('ORAS pinned release checksum mismatch')
PY
  tar -xzf "$tools/$archive" -C "$tools" oras
  "$tools/oras" version
  echo "$tools" >> "$GITHUB_PATH"
  exit 0
fi
[[ $# == 5 && $1 == build ]] || { echo 'usage: custom-release.sh build darwin|linux TAG SHA ABS_OUTPUT' >&2; exit 2; }
platform=$2 tag=$3 sha=$4 out=$5
[[ $out == /* && $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+-custom\.[1-9][0-9]*$ && $sha =~ ^[0-9a-f]{40}$ ]]
[[ $(git rev-parse HEAD) == "$sha" ]]
case "$platform:$(uname -s)" in
 darwin:Darwin) triple=aarch64-apple-darwin ;;
 linux:Linux) triple=aarch64-unknown-linux-gnu ;;
 *) echo 'platform does not match native runner' >&2; exit 2 ;;
esac
[[ $(uname -m) == arm64 || $(uname -m) == aarch64 ]]
[[ ! -e "$out" ]] || { echo 'build output already exists' >&2; exit 1; }
mkdir -p "$out/bin" "$out/share/yazi/completions"
rustup toolchain install 1.97.1 --profile minimal --target "$triple"
export RUSTC RUSTDOC
RUSTC=$(rustup which --toolchain 1.97.1 rustc)
RUSTDOC=$(rustup which --toolchain 1.97.1 rustdoc)
export CARGO_TARGET_DIR="$RUNNER_TEMP/yazi-target"
YAZI_GEN_COMPLETIONS=1 cargo +1.97.1 build --locked --release --target "$triple" -p yazi-fm -p yazi-cli
cp "$CARGO_TARGET_DIR/$triple/release/yazi" "$out/bin/"
cp "$CARGO_TARGET_DIR/$triple/release/ya" "$out/bin/"
for crate in yazi-cli yazi-boot; do
  [[ -d "$crate/completions" ]]
  cp -R "$crate/completions" "$out/share/yazi/completions/$crate"
done
python3 - "$out" "$sha" "$triple" <<'PY'
import json, pathlib, struct, subprocess, sys, tomllib
out, sha, triple = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
base = tomllib.loads(pathlib.Path('Cargo.toml').read_text())['workspace']['package']['version']
for name in ('yazi', 'ya'):
    binary = out / 'bin' / name
    version = subprocess.check_output([binary, '--version'], text=True)
    for expected in (base, sha[:8], 'Debug  : false', triple, '1.97.1'):
        if expected not in version:
            raise SystemExit(f'{name} version missing {expected}: {version}')
    data = binary.read_bytes()[:64]
    if triple.endswith('linux-gnu'):
        if data[:4] != b'\x7fELF' or data[4:6] != b'\x02\x01' or struct.unpack('<H', data[18:20])[0] != 183:
            raise SystemExit('expected ELF ARM64')
        result = subprocess.run(['ldd', binary], capture_output=True, text=True)
        if result.returncode or 'not found' in result.stdout:
            raise SystemExit('unresolved Linux libraries')
    elif data[:4] != b'\xcf\xfa\xed\xfe' or struct.unpack('<I', data[4:8])[0] != 0x100000c:
        raise SystemExit('expected Mach-O ARM64')
    print(version)
toolchains = {key: subprocess.check_output(command, text=True).strip() for key, command in {'rustc': ['rustup', 'run', '1.97.1', 'rustc', '--version'], 'cargo': ['rustup', 'run', '1.97.1', 'cargo', '--version'], 'oras': ['oras', 'version']}.items()}
(out / 'toolchains.json').write_text(json.dumps(toolchains))
PY
