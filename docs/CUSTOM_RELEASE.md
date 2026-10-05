# Custom release provenance

- GitHub fork: `https://github.com/cybito/yazi` (upstream: `https://github.com/sxyazi/yazi.git`). `custom` is the custom build and release source.
- Historical Forgejo source: `https://git.cybit.top/cybit/yazi`; custom ref at migration: `6943a9832675b58071f0059fde4be89ed3f701c9`. It remains intact.
- Historical Forgejo OCI native artifacts `ias-yazi`: Darwin digest `sha256:d3501943350f01e22c5b71dc30a93a72932287fe28512daf58c5791fb1589271`; Linux digest `sha256:994bbe4d994486b5fe58e363864613573a3541371316501965815d9a98dc0834`. These original binary/receipt artifacts remain unchanged and are historical only.
- Initial migrated local checkout was clean at `e9e164cf07198cc25a0e244a3da331a1650a2005`, branch `custom`; remotes: Forgejo `origin` (renamed `forgejo`) and upstream `upstream`.

## Automated custom releases

Only publishing a GitHub Release in `cybito/yazi` triggers `custom-release.yml`; ordinary pushes, tag pushes and pull requests do not publish. Tags use `v<workspace version>-custom.<positive integer>`; this migration's first fully validated asset release uses `v26.9.1-custom.3`, while existing tags/releases remain unchanged. The tag's dereferenced commit must be an ancestor of `origin/custom`, and contain the release workflow and scripts.

The workflow checks out that exact SHA for both `macos-26` ARM64 and `ubuntu-24.04-arm` ARM64. Rust is fixed at **1.97.1**, with matching `RUSTC` and `RUSTDOC`; `YAZI_GEN_COMPLETIONS=1` builds `yazi-fm` and `yazi-cli` with `--locked --release`. It never invokes Debian packaging or upstream publishing workflows. Both `yazi` and `ya` must report the workspace base version, source SHA's first eight characters, `Debug: false`, native triple, and Rust 1.97.1. The custom tag belongs to `release.json`, not a rewritten native version. Both programs' completions are shipped.

Each platform package contains `yazi-<tag>-<platform>-arm64.tar.gz`, `release.json`, and `SHA256SUMS`. GitHub Release asset names are deterministic and collision-free: `<tag>-<platform>-<original-filename>`. Existing assets are downloaded and checked byte-for-byte; differing bytes fail without overwrite, identical assets are reused, and partial platform sets are safely completed. Every downloaded platform set is validated against the exact tag/commit/platform receipt, payload SHA-256/size, and checksums. Files at or above 2 GiB and releases exceeding 1000 assets are rejected before upload. Upload uses the workflow's `GITHUB_TOKEN` and never uses `--clobber`.

`release.json` schema 1 records project, real GitHub source, exact source commit, release tag, platform, architecture, actual Rust/Cargo versions, and archive size/SHA256. It is not nested inside the archive it hashes. `SHA256SUMS` covers the archive and receipt. The package operation retains the isolated, idempotent installer smoke test. After both platforms succeed, the summary job downloads and verifies both complete asset sets, then updates only `<!-- custom-builds:start -->` / `<!-- custom-builds:end -->` in the original Release notes. User-authored notes are retained; managed notes link every downloadable asset. There are no Actions artifact uploads, caches, GHCR or IaC pin updates.

## Download and install without changing the workstation

Managed Release notes link every file. For manual download and validation:

```sh
tag=v26.9.1-custom.3
platform=darwin # use linux on Omarchy ARM64
prefix="$tag-$platform-"
mkdir -p /absolute/path/to/download
cd /absolute/path/to/download
for original in "yazi-$tag-$platform-arm64.tar.gz" release.json SHA256SUMS; do
  gh release download "$tag" --repo cybito/yazi --pattern "$prefix$original"
  mv "$prefix$original" "$original"
done
if [ "$platform" = linux ]; then sha256sum -c SHA256SUMS; else shasum -a 256 -c SHA256SUMS; fi
tar -xzf "yazi-$tag-$platform-arm64.tar.gz"
sh yazi/install.sh --prefix /absolute/path/to/prefix
/absolute/path/to/prefix/bin/yazi --version
/absolute/path/to/prefix/bin/ya --version
```

The installer defaults to `$HOME/.local` when no prefix is supplied; an explicit prefix must be absolute. It copies only `bin`/`share`, checks the entire destination before copying, rejects symlink paths and differing existing files, leaves identical files untouched on reruns, and does not restart services, replace configuration, uninstall packages or modify shell startup. Completions are in `<prefix>/share/yazi/completions/{yazi-cli,yazi-boot}`; configure the chosen shell's completion search path explicitly.

## Maintainer setup and verification

No Forgejo environment, registry credential, or PAT is needed. Publication and summary use the workflow's built-in GitHub token; `contents: write` is scoped to those two jobs. Do not add a separate credential. Keep inherited workflows disabled and enable `custom-release.yml` as the intended publication workflow.

Public helper CLI:

```text
check --tag TAG --commit SHA --platform darwin|linux --output-dir ABS
pack --tag TAG --commit SHA --platform darwin|linux --input-dir ABS --output-dir ABS
publish --directory ABS
validate-event
summarize
self-test
```

`check` emits JSON `exists:false` for a missing platform set; for an existing full set it emits asset names after downloading and validating them. `publish` verifies existing bytes, uploads only missing assets without overwrite, downloads all three assets again and validates receipt identity, payload and checksums. `validate-event`, `summarize` and `self-test` are internal workflow commands. Self-test exercises real isolated Git ancestry, invalid and malicious release tags, asset checksum and readback validation. The runner also executes both versions and checks Mach-O ARM64 or ELF machine 183; Linux checks unresolved shared libraries.

Historic Forgejo references above describe prior source and immutable artifacts only; active build publication and download use GitHub Release assets.
