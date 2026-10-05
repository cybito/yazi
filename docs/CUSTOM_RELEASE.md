# Custom release provenance

- GitHub fork: `https://github.com/cybito/yazi` (upstream: `https://github.com/sxyazi/yazi.git`). `custom` is the custom build and release source.
- Historical Forgejo source: `https://git.cybit.top/cybit/yazi`; custom ref at migration: `6943a9832675b58071f0059fde4be89ed3f701c9`. It remains intact.
- Historical OCI native artifact `git.cybit.top/cybit/ias-yazi`: Darwin digest `sha256:d3501943350f01e22c5b71dc30a93a72932287fe28512daf58c5791fb1589271`; Linux digest `sha256:994bbe4d994486b5fe58e363864613573a3541371316501965815d9a98dc0834`. Original binary/receipt artifacts remain unchanged.
- Custom install packages are a distinct artifact type in `ias-yazi`. Immutable download: `oras pull git.cybit.top/cybit/ias-yazi@sha256:<digest>`.
- Initial migrated local checkout was clean at `e9e164cf07198cc25a0e244a3da331a1650a2005`, branch `custom`; local head: `custom`; remotes: Forgejo `origin` (renamed `forgejo`) and upstream `upstream`. Forgejo advanced by 3 commits to the recorded revision.

## Automated custom releases

Only publishing a GitHub Release in `cybito/yazi` triggers `custom-release.yml`;
ordinary pushes, tag pushes and pull requests do not publish packages. Tags use
`v<workspace version>-custom.<positive integer>`; the initial planned tag is
`v26.9.1-custom.1` (choose the next unused integer if occupied). The tag's
dereferenced commit must be an ancestor of `origin/custom`, and must contain
the release workflow and scripts. Release assets must remain empty.

The workflow checks out that exact SHA for both `macos-26` ARM64 and
`ubuntu-24.04-arm` ARM64. Rust is fixed at **1.97.1**, with matching `RUSTC`
and `RUSTDOC`; `YAZI_GEN_COMPLETIONS=1` builds `yazi-fm` and `yazi-cli`
with `--locked --release`. It never invokes Debian packaging or upstream
publishing workflows. Both `yazi` and `ya` must report the workspace base
version, source SHA's first eight characters, `Debug: false`, native triple,
and Rust 1.97.1. The custom tag belongs to `release.json`, not to a falsely
rewritten native version. Both programs' completions are shipped.

Before building, the anonymous registry check pulls and verifies an existing
tag by digest. A matching identity skips rebuilding; mismatched identities,
authentication errors and network failures fail the job. Only explicit
`MANIFEST_UNKNOWN`/`NAME_UNKNOWN` means absent. The package tag is
`<release-tag>-<darwin|linux>-arm64`. Successful platforms remain published
when another platform fails, so rerunning fills only the missing platform.

Each install package is `yazi-<release-tag>-<darwin|linux>-arm64.tar.gz`
and includes both executables, completions, README, licenses and `install.sh`.
The OCI artifact type is `application/vnd.cybito.install-package.v1`;
historical `application/vnd.ias.native.v1` artifacts and receipts are not
changed or reused. `release.json` schema 1 records project, real GitHub source,
exact source commit, release tag, platform, architecture, actual toolchain
versions, and archive size/SHA256. It is an OCI sidecar, not nested inside
the archive it hashes. `SHA256SUMS` covers the archive and receipt.
Layers have explicit JSON, text or gzip media types.

Publishing constructs a local OCI layout, obtains its immutable digest, copies
it to Forgejo, then independently pulls and checks manifest, config descriptor,
all layers, receipt and checksums. Creation time is the source commit's UTC time.
After both platforms succeed, the anonymous summary job validates both and
updates only `<!-- custom-builds:start -->` / `<!-- custom-builds:end -->`
in the original Release notes. User notes are retained. There are no GitHub
Release assets, Actions artifact uploads, caches, GHCR or IaC pin updates.

## Download and install without changing the workstation

Use the immutable digest printed in the Release notes:

```sh
mkdir -p /absolute/path/to/download
oras pull git.cybit.top/cybit/ias-yazi@sha256:<digest> \
  --output /absolute/path/to/download
cd /absolute/path/to/download
shasum -a 256 -c SHA256SUMS
tar -xzf yazi-v26.9.1-custom.1-darwin-arm64.tar.gz
sh yazi/install.sh --prefix /absolute/path/to/prefix
/absolute/path/to/prefix/bin/yazi --version
/absolute/path/to/prefix/bin/ya --version
```

For Omarchy ARM64 choose the `linux-arm64` archive instead. The installer
defaults to `$HOME/.local` when no prefix is supplied; an explicitly provided
prefix must be absolute. It copies only `bin`/`share`, checks the entire
destination before copying, rejects symlink paths and differing existing
files, and leaves identical files untouched on reruns. It does not restart
services, replace configuration, uninstall packages or modify shell startup.
Completions are in `<prefix>/share/yazi/completions/{yazi-cli,yazi-boot}`;
configure the chosen shell's completion search path explicitly.

For stronger verification than a standalone checksum file, use the helper:

```sh
python3 .github/scripts/package-release.py verify \
  --reference git.cybit.top/cybit/ias-yazi@sha256:<digest> \
  --output-dir /absolute/path/to/new-empty-directory
```

## Maintainer setup and verification

Disable inherited workflows and enable only `custom-release.yml`. Configure
the GitHub `forgejo-registry` environment to allow tags `v*-custom.*`, with no
branch deployment policy. A human must create a dedicated Forgejo PAT named
`github-custom-builds` for user `cybit` with package write permission, preferably
public-only, and set its environment secret `FORGEJO_REGISTRY_TOKEN`.
Package permission applies to the owner namespace, not merely these six
packages. Do not extract existing OAuth or Docker credential-helper secrets.
Associate `ias-yazi` with Forgejo `cybit/yazi` using package settings;
the OCI source annotation remains the real GitHub source URL.

The token is injected only after build, smoke and isolated installation succeed.
ORAS uses explicit registry configs, stdin login, a private temporary directory
and mode-0600 auth file; cleanup runs even after failure. No PAT is exposed to
the final notes job. ORAS 1.3.3 downloads are checked against its official
versioned release checksum file. Rustup installs the exact 1.97.1 toolchain.

Public helper CLI:

```text
check --tag TAG --commit SHA --platform darwin|linux --output-dir ABS
pack --tag TAG --commit SHA --platform darwin|linux --input-dir ABS --output-dir ABS
publish --directory ABS --registry-config ABS
verify --reference OCI_DIGEST_REF --output-dir ABS
```

`check` emits JSON `exists` and immutable `reference` when present; `pack`
emits `directory`; `publish` emits `reference` and `digest`; `verify` emits
the verified receipt. `validate-event`, `summarize` and `self-test` are internal
workflow commands. The latter exercises real isolated Git ancestry, invalid
and malicious release tags, registry failure classification, and wrong-receipt
rejection. The runner exercises archive installation twice in a temporary
prefix, executes both versions, and checks Mach-O ARM64 or ELF machine 183;
Linux also checks unresolved shared libraries.

Local migration checks passed: Python parsing, `bash -n`, YAML parsing, and
`python3 .github/scripts/package-release.py self-test`. Hosted ARM64 builds,
external publication, anonymous digest pulls, installer checks on runner-built
payloads, and repeat-run digest stability remain unverified. Verify ordinary
pushes do not create release runs, Release assets remain empty, and no Actions
artifacts or caches are added. Historical digests must remain readable; this
workflow does not deploy IaC.
