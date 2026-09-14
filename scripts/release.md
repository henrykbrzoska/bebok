# Releasing Bebok - CI mechanics

`.github/workflows/release.yml` is the only producer of releases. It runs on:

| Trigger | Result |
|---|---|
| `pull_request` into `main` from `release/**` | **draft** release named after the manifest version (the pre-release testers install) |
| `push` to `main` whose manifest version has no tag | tags the commit and **publishes** (the merged release PR); any other push stops in `preflight` |
| `push` of a tag `X.Y.Z` / `vX.Y.Z` | publishes (manual path) |
| `workflow_dispatch` | same pipeline, draft by default (pipeline dry runs) |

One run produces every platform bundle, a merged `SHA256SUMS.txt`, the
updater manifest `latest.json`, and creates or updates the GitHub Release.

The process itself (`npm run release` -> test the draft -> merge) and what to
do when a run fails is in [CONTRIBUTING.md#releasing](../CONTRIBUTING.md#releasing).
This file covers the mechanics only: what the workflow builds, signing, the
updater feed, dry runs and the local equivalent of one matrix leg.

## What gets built

| Job | Runner | Target | Assets |
|---|---|---|---|
| `build (linux-x64)` | `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | `bebok_X.Y.Z_amd64.deb`, `bebok_X.Y.Z_amd64.AppImage`, `bebok-X.Y.Z-linux-x64-portable.tar.gz` |
| `build (windows-x64)` | `windows-latest` | `x86_64-pc-windows-msvc` | `bebok_X.Y.Z_x64_en-US.msi`, `bebok_X.Y.Z_x64-setup.exe` (NSIS), `bebok-X.Y.Z-windows-x64-portable.zip` |
| `build (macos-arm64)` | `macos-15` | `aarch64-apple-darwin` | `bebok_X.Y.Z_aarch64.dmg`, `bebok_X.Y.Z_aarch64.app.tar.gz` |
| `build (macos-x64)` | `macos-15-intel` | `x86_64-apple-darwin` | `bebok_X.Y.Z_x64.dmg`, `bebok_X.Y.Z_x64.app.tar.gz` |

Every leg also uploads the headless engine as
`bebok-server-X.Y.Z-<platform>[.exe]` (browser mode / remote engine for the
mobile client). Portable archives contain `bebok-desktop` + `bebok-server`
side by side; keep them together (the shell resolves the sidecar next to its
own executable). The `release` job merges the per-OS checksum files into
`SHA256SUMS.txt`.

GitHub retired the Intel `macos-13` runner; `macos-15-intel` is the current
x86_64 label (free for public repositories).

### Signing

Code signing (Windows Authenticode, GPG) is optional: each block is skipped
with a `::notice::` in the job log when its secrets are missing
(Settings -> Secrets and variables -> Actions). The **updater signature is
mandatory** - the build job fails without `TAURI_SIGNING_PRIVATE_KEY`, see
[Auto-update](#auto-update).

| Secrets | Effect when present |
|---|---|
| `WINDOWS_CERT_PFX_BASE64`, `WINDOWS_CERT_PASSWORD` | Authenticode-signs `bebok-server.exe` (sidecar), `bebok-desktop.exe`, the MSI and the NSIS installer with `signtool` (SHA-256, RFC 3161 timestamp from `http://timestamp.digicert.com`). The PFX is imported into the runner's `CurrentUser\My` store for the job and removed afterwards. Create the secret with `base64 -w0 cert.pfx` (or `[Convert]::ToBase64String([IO.File]::ReadAllBytes('cert.pfx'))`). |
| `GPG_PRIVATE_KEY`, `GPG_PASSPHRASE` | ASCII-armored private key (`gpg --armor --export-secret-keys KEYID`). Produces detached `.asc` signatures for the `.deb`, `.AppImage`, portable `.tar.gz` and the merged `SHA256SUMS.txt`, embeds a GPG signature in the AppImage (`SIGN=1`/`SIGN_KEY` for Tauri's appimagetool) and attaches the public key as `bebok-release-signing-key.asc`. |
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | **Required.** Enables `bundle.createUpdaterArtifacts` so Tauri emits minisign `.sig` files next to the installers / AppImage / `.deb` / `.app.tar.gz`; the `release` job turns them into `latest.json`. The password secret may be empty when the key has none. |

macOS builds are **never** signed or notarized (no Apple Developer ID in
CI). Consequences for users, also printed in the release notes:

- **macOS**: Gatekeeper blocks the first launch. Right-click the app ->
  *Open*, or `xattr -dr com.apple.quarantine /Applications/bebok.app`.
- **Windows** (when unsigned): SmartScreen shows "Windows protected your PC";
  *More info -> Run anyway*.
- **Linux**: no warnings; `.deb` and AppImage work unsigned. The AppImage and
  portable build need WebKitGTK 4.1 on the host.

## Android (WP-M3)

`.github/workflows/android.yml` builds the APK on every PR/push touching
`client/**` or `engine/**` (arm64-v8a + x86_64, the latter only so the CI
emulator can run it); `.github/workflows/release.yml`'s `android` job runs on
a tag (arm64-v8a only - see PLAN-1.6-MOBILE.md #9 on APK size) and attaches
`bebok-X.Y.Z-android-arm64.apk` to the release, with its checksum folded
into `SHA256SUMS.txt`.

The embedded engine is packaged as a *native library* (F10-29):
`client/scripts/bundle-android.sh` writes the stripped, per-ABI
`bebok-server` to `client/android/app/src/main/jniLibs/<abi>/libbebok_server.so`
(plus `libmksh.so` when mksh could be cross-compiled), `app/build.gradle`
packages `jniLibs` with `useLegacyPackaging` (= `extractNativeLibs="true"`)
and limits the APK's ABIs to the ones that actually carry an engine, and
`EngineLauncherPlugin` execs it from `ApplicationInfo.nativeLibraryDir`. This
is the only exec-allowed location for an untrusted app on Android 10+; the
earlier "copy the asset into `files/bin`" approach dies with
`error=13, Permission denied` (W^X) on Android 16. A `unzip -l app.apk | grep
lib/` should therefore list `lib/arm64-v8a/libbebok_server.so` (and
`lib/x86_64/...` for the PR build); an APK built without running the bundle
script has no engine and only works in Remote mode.

`versionName`/`versionCode` are **not** set in `client/android/app/build.gradle`
directly - the file reads `client/package.json`'s `version` at Gradle
configuration time (`versionCode = major*10000 + minor*100 + patch`), so
`npm run version:bump` is the only place a release version is written, same
as every other platform. `preflight` has a lightweight guard against that
derivation being replaced with a hard-coded string later.

### Signing

| Secrets | Effect when present |
|---|---|
| `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD` | Decodes the keystore to `$RUNNER_TEMP` and passes it to `gradlew assembleRelease` via `-Pbebok.keystore=… -Pbebok.keystorePassword=… -Pbebok.keyAlias=… -Pbebok.keyPassword=…` (read by `signingConfigs.release` in `app/build.gradle`). Create the secret with `base64 -w0 release.keystore` (or the PowerShell `[Convert]::ToBase64String(...)` equivalent used for the Windows cert). |

Without all four secrets, `assembleRelease` still succeeds: the `release`
build type falls back to the auto-generated **debug** signing config and the
output filename gets a `-debugkey` suffix (`android.yml`'s PR artifact) or a
log notice (the release job keeps the published asset name
`bebok-X.Y.Z-android-arm64.apk` either way, with a note in the release
description instead - a debug-signed APK cannot be upgraded in place by a
later release-keystore-signed build, so uninstall it first if you installed
one from a PR run).

### Local equivalent

```bash
cd client
BEBOK_ANDROID_ABIS="arm64-v8a x86_64" npm run android:bundle   # Git Bash/WSL; needs
                                                                 # ANDROID_NDK_HOME (r27)
                                                                 # + rustup targets
                                                                 # aarch64-linux-android,
                                                                 # x86_64-linux-android
npx ng build
npx cap sync android
cd android && ./gradlew assembleRelease testDebugUnitTest
```

`bundle-android.sh` detects the NDK host tag (`linux-x86_64` /
`darwin-x86_64` / `windows-x86_64`) itself; on Windows it must run under Git
Bash or WSL (it is a bash script), and it resolves the NDK's Windows
toolchain wrapper (`.cmd`) explicitly - the extension-less
`<target><api>-clang` NDK ships on Windows is a POSIX shell script that a
natively-spawned `rustc`/`cc` cannot exec directly.

Maestro flows live in `client/maestro/**` (`npm run android:maestro` against
whatever `adb` currently targets) - see `client/maestro/README.md`.

## Auto-update

The desktop app checks
`https://github.com/henrykbrzoska/bebok/releases/latest/download/latest.json`
shortly after start and every 6 h (About -> *Check for updates* does it on
demand) and installs the signed bundle for its platform through
`tauri-plugin-updater`. Browser mode and `tauri dev` builds only detect a
newer tag via the GitHub API and link to the release page.

- `latest.json` is composed by `scripts/latest-json.mjs` in the `release`
  job from the collected artifacts: `windows-x86_64[-nsis|-msi]`,
  `linux-x86_64[-appimage|-deb]`, `darwin-aarch64`, `darwin-x86_64`, each with
  its `.sig`. Any missing bundle or signature fails the job - a release
  without a usable manifest must not become *Latest*.
- Installed apps verify every download against the public key in
  `client/src-tauri/tauri.conf.json` (`plugins.updater.pubkey`). The matching
  private key lives outside the repo (generated with
  `npx tauri signer generate -w ~/.tauri/bebok.key`; store the file and the
  password in a password manager). Losing it means installed copies can never
  update again - users would have to reinstall by hand. Rotating it requires
  one release signed with the old key that ships the new `pubkey`.
- The shell kills the `bebok-server` sidecar before the installer runs
  (`on_before_exit`) - otherwise the Windows installer cannot overwrite the
  locked `bebok-server.exe`. Engine and GUI must always come from the same
  release; the About screen flags a version mismatch.
- **Never upload release assets by hand.** The updater compares
  `latest.json` against the tag; a hand-made release (like `1.6.1`, published
  with `1.6.0` binaries after `preflight` rejected the tag) either has no
  manifest or announces a version the binaries do not carry, and every
  installed app keeps re-offering it.
- Pre-releases (`X.Y.Z-1`) are never *Latest* on GitHub, so they are the
  way to test the whole update path end to end on all four platforms before
  a real release.

## Test run without tagging (`workflow_dispatch`)

Actions -> **Release** -> *Run workflow*:

- **Use workflow from**: the branch to build.
- **tag**: leave empty to build the selected branch and name the release
  after the manifest version, or give an existing tag/ref.
- **draft**: keep `true` (default) for a test run. A draft release does not
  create the tag and is invisible to users; delete it when done.

CLI equivalents:

```bash
# draft release from the current main, tag = version in client/package.json
gh workflow run release.yml --ref main -f draft=true

# build a specific tag/ref as a draft (do not point this at a tag that is
# already released - see the note below)
gh workflow run release.yml --ref main -f tag=1.5.0-1 -f draft=true

gh run watch
gh release list
gh release delete 1.5.0-1 --yes   # remove the test draft
```

`preflight` still requires the manifests to agree with the tag / with each
other. A manual run **refuses to touch a release that is already published**
for that tag (the `release` job fails with a clear error, the build artifacts
stay attached to the workflow run), because `softprops/action-gh-release`
would otherwise overwrite its assets and re-apply the `draft` flag. So to test
the whole pipeline end to end, bump the version on the branch first (e.g.
`npm run version:bump -- 1.5.0-1`, commit) and dispatch from that branch;
an existing *draft* release for the tag is updated in place.

`gh workflow run release.yml` needs the workflow to exist on the ref you pass
(`--ref`), i.e. after this file has landed on `main`.

## Local equivalent of one matrix leg

```bash
# repo root - host target triple, default bundles per OS (nsis,msi / deb,appimage / dmg),
# prints every artifact path with its SHA256; see `node scripts/bebok.mjs --help`
npm run full-build-app                       # or: -- --bundles nsis  /  -- --skip-tauri
```

which runs the same steps as the workflow (unsigned):

```bash
cd engine && cargo build --release --locked -p bebok-server --target x86_64-pc-windows-msvc
cd ../client && npm ci
npm run sidecar:copy -- --target x86_64-pc-windows-msvc
npx tauri build --ci --target x86_64-pc-windows-msvc --bundles msi,nsis
# bundles: client/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/{msi,nsis}
```

`npm run doctor` (root) checks the toolchain and the OS-level Tauri
dependencies before you try; `npm run full-build-dev` is the matching
development loop (engine + `ng serve` with the token wired in).

Lint the workflows before pushing changes to them:

```bash
npx --yes -p actionlint-cli actionlint   # or: go install github.com/rhysd/actionlint/cmd/actionlint@latest
```
