# Maestro flows (Android)

Smoke tests for the Bebok Android app, run against an emulator or a real
device with the app already installed. Maestro drives the app through its
accessibility tree, so selectors here are **text-based** (`assertVisible`
with the visible string) rather than resource ids - the Capacitor WebView
renders the whole UI as DOM content, which does not expose Android view ids.

## Flows

- `smoke.yaml` - launches the app, waits for the embedded engine (F10-9) to
  report the connected state (`status.live` = "engine live" in
  `client/src/i18n/en.ts`), and takes a screenshot. This is the baseline
  "the app boots and the engine comes up" check.

- `chat-local.yaml` (WP-M5) - fresh state -> onboarding -> "Chat locally" ->
  quick session -> `[write]` prompt -> permission prompt answered -> mock
  reply streamed -> relaunch -> history still listed. Needs a *debuggable*
  build with the mock provider on (Settings -> This device -> "Mock
  provider", or `adb shell setprop debug.bebok.provider_mock 1`).

Later packages add more flows next to these (`remote-mirror.yaml`, etc. -
see `analysis/PLAN-1.6-MOBILE.md` section 8).

## Running locally

Prerequisites: an Android emulator (API 34 x86_64 recommended) or a
connected device, with the app already built and installed
(`npm run android:bundle && npx cap sync android && cd android &&
./gradlew installDebug`), and the [Maestro CLI](https://maestro.mobile.dev)
on your `PATH` (`curl -Ls "https://get.maestro.mobile.dev" | bash` on
Linux/macOS/WSL; there is no native Windows build, so on Windows either run
Maestro from WSL against an emulator started in Windows-hosted `adb`, or rely
on the CI job below).

```
cd client
npm run android:maestro
```

That runs `maestro test maestro/smoke.yaml` against whatever device/emulator
`adb` currently targets (`adb devices` to check).

## Running in CI

`.github/workflows/android.yml`'s `emulator-smoke` job boots an API 34
x86_64 emulator (`reactivecircus/android-emulator-runner`), installs the APK
built by the `build` job, and runs every `*.yaml` flow in this directory. On
failure, Maestro's own screenshot/video artifacts (and the explicit
`takeScreenshot: WP-M3-smoke` in `smoke.yaml`) are uploaded as a workflow
artifact named `maestro-smoke-results` - check there first when a run fails
before re-running.
