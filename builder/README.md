# Local builder

Build the release artifacts on your machine, the way `release.yml` does,
without installing toolchains on the host.

```bash
npm run builder                                   # asks: which targets, which version
npm run builder -- --targets linux,android --version 1.8.0-test1 --yes
npm run builder:up                                # build the images + start the idle stack
npm run builder:status                            # which builder containers are running
npm run builder:down                              # stop the stack (caches stay)
```

The Docker side is a long-lived compose stack (project `bebok-builder`):
three idle containers, `bebok-builder-linux` / `-windows` / `-android`,
that show up in Docker Desktop / OrbStack and stay running between builds.
`npm run builder` starts the ones it needs (building the images the first
time) and runs each build as a `docker compose exec … bebok-build` inside
the matching container; the containers keep running afterwards, ready for
the next build. Manually: `docker compose -f builder/compose.yml exec -e
VERSION=1.8.0-test1 linux bebok-build`.

The script checks the host first and the menu shows how (or whether) each
target can be built there: Linux and Android always go through Docker;
Windows is native on a Windows host (MSI + NSIS, exactly CI's leg) and a
Docker cross-build elsewhere (NSIS only); macOS is native on a Mac and
unavailable anywhere else.

| Target | Where | Output |
|---|---|---|
| `linux` | Docker (`ubuntu:22.04`, same deps as the CI leg) | `bebok_<v>_amd64.deb`, `.AppImage`, portable `.tar.gz`, headless `bebok-server` |
| `windows` | Docker, cross-compiled with `cargo-xwin` + NSIS (Tauri's documented Linux->Windows path; MSI needs Windows) | `bebok_<v>_x64-setup.exe`, portable `.zip`, `bebok-server.exe` |
| `android` | Docker (JDK 21, SDK 35, NDK r27c) | `bebok-<v>-android-arm64.apk` |
| `macos` | natively, macOS host only (no container can build .app/.dmg) | `bebok_<v>_<arch>.dmg`, `.app.tar.gz` |

Everything lands in `~/bebok-dist/<version>/<platform>/` (override with
`--out` or `BEBOK_OUT`), plus a merged `SHA256SUMS.txt` and - when all four
platforms with `.sig` files are present - a `latest.json` like CI's.

The checkout is mounted read-only; each build works on a private copy where
the version is bumped, so your manifests stay untouched. Version suggestion:
`<current>-testN`, N counting up from what is already in the output dir.

Signing: if `~/.tauri/bebok.key` exists the bundles get updater `.sig`
files (installed apps accept them as updates); if
`~/.tauri/bebok-android.keystore` + `.password` exist the APK is signed with
the release key (an APK signed with the debug key cannot be upgraded in
place by a signed one - uninstall first). Both are the same secrets CI uses.

First run pulls `ubuntu:22.04` and installs the toolchains into the images
(~3-5 GB, ~10-15 min); later runs reuse the images and the cargo / gradle
caches (named volumes), so a rebuild is a few minutes. On Apple silicon the
images run as `linux/amd64` under Rosetta - slower than native, identical
output. `docker volume rm` the `bebok*` volumes to start clean.
