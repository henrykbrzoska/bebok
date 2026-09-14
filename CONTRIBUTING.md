# Contributing to Bebok

Short version of how work lands in this repository. The contributor/agent
orientation guide is [AGENTS.md](AGENTS.md); the CI/release internals are in
[scripts/release.md](scripts/release.md).

## Day-to-day

- Work on a branch, one git worktree per work package; merges to the default
  branch go through a pull request.
- Before pushing: `cd engine && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
  and `cd client && npm run build && npm test`.
- i18n keys are added to **all 12** dictionaries in `client/src/i18n/`
  (`en.ts` is the reference; a missing key elsewhere is a compile error).
- No `Co-Authored-By` trailers in commit messages.
- Keep `CHANGELOG.md` current: add your change under `## Unreleased` in the
  same PR. The release process turns that section into `## X.Y.Z — date`.

## Releasing

A release is **only** ever produced by `.github/workflows/release.yml`.
Installed desktop apps update themselves from what that workflow publishes
(`latest.json` + signed bundles), so a hand-made release is not just untidy -
it silently breaks auto-update for everyone. Never upload, replace or delete
release assets by hand.

### Checklist

1. **Default branch is green** in CI (Actions -> CI) and contains everything
   that should ship. Work from a clean checkout of it.
2. **Changelog**: rename `## Unreleased` in `CHANGELOG.md` to
   `## X.Y.Z — YYYY-MM-DD` and tidy the entries. The release job copies this
   section into `latest.json` as the update notes users see in the app.
3. **Bump the version** in all seven manifests/lockfiles with one command:

   ```bash
   cd client && npm run version:bump -- X.Y.Z
   ```

   Do not edit the version by hand anywhere - `preflight` fails the release
   if `client/package.json`, `client/src-tauri/tauri.conf.json`, both
   `Cargo.toml` and both `Cargo.lock` files disagree with the tag.
4. **Commit and tag** - the tag is the bare version, on the bump commit:

   ```bash
   git add -A
   git commit -m "chore: bump version to X.Y.Z"
   git tag X.Y.Z
   git push origin HEAD X.Y.Z
   ```

5. **Watch Actions -> Release** (~25-30 min): `preflight` (tag == manifests)
   -> four build legs (linux-x64, windows-x64, macos-arm64, macos-x64) ->
   `release` (merges `SHA256SUMS.txt`, composes `latest.json`, publishes the
   GitHub Release). A tag push publishes immediately, not as a draft.
6. **Verify** on the Releases page: installers for all four platforms, their
   `.sig` files, `SHA256SUMS.txt` and `latest.json`; then

   ```bash
   curl -sL https://github.com/henrykbrzoska/bebok/releases/latest/download/latest.json | head -5
   ```

   must show the new version. Finally, on a machine with the previous version
   installed: topbar version chip -> *Check for updates* -> the new version is
   offered -> *Install & restart* works.
7. Done. Nothing else to publish: installed apps pick the release up within
   10 s of their next start or at their next 6-hourly check.

### If the workflow fails

- `preflight` failed (tag/manifest mismatch): fix with `npm run version:bump`,
  commit, then either move the tag (`git tag -f X.Y.Z && git push -f origin X.Y.Z`)
  if nothing was published yet, or bump to the next patch version and tag
  that. Do **not** create the release by hand from local builds - that is how
  `1.6.1` ended up as *Latest* with `1.6.0` binaries and no manifest.
- A build leg failed (runner hiccup): re-run the failed jobs from the Actions
  UI; the `release` job runs once all four legs are green.
- The `release` job failed at "Compose latest.json": a signed bundle is
  missing. Check that `TAURI_SIGNING_PRIVATE_KEY` is set (Settings ->
  Secrets and variables -> Actions) - the updater key is mandatory.

### Pre-releases and dry runs

- Version suffixes must be **numeric**: `X.Y.Z-1`, `X.Y.Z-2` … The MSI
  bundler rejects `-rc.1` / `-beta.1`, so `preflight` rejects them too. A
  suffixed tag is published as a *pre-release*: not *Latest*, never offered
  to installed apps.
- To test the pipeline without publishing: bump on a branch, then
  `gh workflow run release.yml --ref <branch> -f draft=true`. Delete the
  draft (`gh release delete X.Y.Z-N --yes`) when done.

### Keys and secrets

`TAURI_SIGNING_PRIVATE_KEY` (+ optional `_PASSWORD`) signs every bundle;
installed apps verify against `plugins.updater.pubkey` in
`client/src-tauri/tauri.conf.json`. The private key lives outside the repo -
keep a backup in a password manager. Losing it means installed copies can
never auto-update again. Details, rotation and the optional Authenticode/GPG
signing: [scripts/release.md](scripts/release.md).
