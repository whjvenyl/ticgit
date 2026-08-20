# Releasing

Releases are triggered by pushing a `v*` tag. The GitHub Actions workflow
(`.github/workflows/release.yml`) builds binaries for five targets, packages
them, and creates a GitHub Release with auto-generated notes.

## Prerequisites

- The `Release` workflow is enabled under **Settings → Actions** on GitHub.
- `.github/workflows/release.yml` is committed and present in the tagged
  commit (GitHub runs the workflow file that exists *at the tag*, not on
  `master`).
- All changes you want in the release are committed and pushed to `master`.

## Cut a release

### 1. Bump the version

```sh
./scripts/bump-version.sh 0.4.0
```

This updates:

- `version` in the root `Cargo.toml`
- `ticgit-lib` path-dependency versions in `crates/ticgit/Cargo.toml`
- version strings in `docs/index.html`

### 2. Verify

```sh
cargo check
cargo test
```

### 3. Commit the version bump

```sh
git add -A
git commit -m "Bump version to 0.4.0"
```

### 4. Tag and push

```sh
git tag v0.4.0
git push origin master
git push origin v0.4.0
```

The tag push triggers the Release workflow.

### 5. Watch the workflow

```sh
gh run watch
```

Or view it at
`https://github.com/whjvenyl/ticgit/actions/workflows/release.yml`.

When the workflow finishes, the release appears at
`https://github.com/whjvenyl/ticgit/releases` with binaries attached:

- `ticgit-x86_64-unknown-linux-gnu.tar.gz`
- `ticgit-aarch64-unknown-linux-gnu.tar.gz`
- `ticgit-x86_64-apple-darwin.tar.gz`
- `ticgit-aarch64-apple-darwin.tar.gz`
- `ticgit-x86_64-pc-windows-msvc.zip`

## Choosing a version number

- **Patch** (`0.4.1`) — bug fixes, small maintenance changes.
- **Minor** (`0.5.0`) — new features, or a significant fork-era change.
- **Major** (`1.0.0`) — breaking changes or stability milestone.

The `-next` suffix (e.g. `0.3.1-next`) marks in-progress development
versions. Do not tag or release a `-next` version — cut a real version
once the work is ready.

## Pre-release tags

The workflow matches any `v*` tag, so pre-release tags like `v0.4.0-rc.1`
also trigger it. GitHub treats the `-rc.1` suffix as a pre-release
automatically.

## What happens after release

- `ti update` checks
  `https://github.com/whjvenyl/ticgit/releases/latest` for newer versions.
- The install script (`docs/install`) downloads from the same releases.
