#!/usr/bin/env bash
# Container entrypoint for the builder images (see Dockerfile / compose.yml).
#
#   /repo     the checkout, read-only (bind mount)
#   /work     a private copy of it (rsync, minus node_modules/target/dist)
#   /out      artifacts land in /out/<VERSION>/<platform>/ (bind mount, outside the repo)
#   /secrets  ~/.tauri from the host: bebok.key (updater), bebok-android.keystore (+ .password)
#   /cache/*  named volumes: cargo registry, per-target cargo target dirs, npm, gradle, xwin
#
# VERSION (env) is written into the seven manifests of the private copy the
# same way `npm run version:bump` does on a release branch - the checkout
# itself is never touched. The steps mirror release.yml step for step so the
# artifact names match what CI publishes.
set -euo pipefail

: "${BEBOK_BUILD_TARGET:?set by the image}"
: "${VERSION:?VERSION is required (e.g. 1.8.0-test1)}"
OUT="/out/${VERSION}"
log() { printf '\033[1m[builder:%s]\033[0m %s\n' "$BEBOK_BUILD_TARGET" "$*"; }

# ---- 1. private working copy -------------------------------------------
log "syncing /repo -> /work"
rsync -a --delete \
  --exclude node_modules --exclude target --exclude dist --exclude .angular \
  --exclude 'client/src-tauri/binaries' --exclude 'client/android/app/build' \
  --exclude 'client/android/.gradle' --exclude '.wrangler' --exclude 'relay/node_modules' \
  /repo/ /work/
cd /work

# Cargo target dirs live on named volumes so rebuilds are incremental; the
# paths the scripts expect (engine/target, client/src-tauri/target) are links.
mkdir -p "/cache/target-${BEBOK_BUILD_TARGET}/engine" "/cache/target-${BEBOK_BUILD_TARGET}/tauri"
ln -sfn "/cache/target-${BEBOK_BUILD_TARGET}/engine" engine/target
ln -sfn "/cache/target-${BEBOK_BUILD_TARGET}/tauri" client/src-tauri/target
export npm_config_cache=/cache/npm
export GRADLE_USER_HOME=/cache/gradle

# ---- 2. version ---------------------------------------------------------
log "version ${VERSION}"
node client/scripts/bump-version.mjs "${VERSION}" > /dev/null

# ---- 3. secrets (optional, same names as the CI secrets) ----------------
if [ -s /secrets/bebok.key ]; then
  export TAURI_SIGNING_PRIVATE_KEY="$(cat /secrets/bebok.key)"
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
  log "updater signing: on (/secrets/bebok.key)"
else
  log "updater signing: OFF - no /secrets/bebok.key, installed apps will not accept these bundles as updates"
fi

# ---- 4. client deps -----------------------------------------------------
log "npm ci (client)"
(cd client && npm ci --no-audit --no-fund > /dev/null)

mkdir -p "$OUT"
case "$BEBOK_BUILD_TARGET" in
# ==========================================================================
linux)
  TARGET=x86_64-unknown-linux-gnu; PLATFORM=linux-x64; P="$OUT/$PLATFORM"; mkdir -p "$P"
  log "engine: cargo build --release --target $TARGET"
  (cd engine && cargo build --release --locked -p bebok-server --target "$TARGET")
  (cd client && npm run sidecar:copy -- --target "$TARGET" > /dev/null)
  cfg='{}'
  [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ] && cfg='{"bundle":{"createUpdaterArtifacts":true}}'
  echo "$cfg" > client/tauri-release-override.json
  log "tauri build (deb,appimage)"
  (cd client && npx tauri build --ci --target "$TARGET" --bundles deb,appimage --config tauri-release-override.json)
  bundle="client/src-tauri/target/$TARGET/release/bundle"; bin="client/src-tauri/target/$TARGET/release"
  app="$bin/bebok-desktop"; [ -f "$app" ] || app="$bin/bebok"
  cp "engine/target/$TARGET/release/bebok-server" "$P/bebok-server-$VERSION-$PLATFORM"
  cp "$bundle"/deb/*.deb "$bundle"/appimage/*.AppImage "$P"/
  cp "$bundle"/appimage/*.AppImage.sig "$bundle"/deb/*.deb.sig "$P"/ 2>/dev/null || true
  stage="/tmp/stage/bebok-$VERSION"; rm -rf /tmp/stage; mkdir -p "$stage"
  cp "$app" "engine/target/$TARGET/release/bebok-server" LICENSE README.md "$stage"/
  tar -czf "$P/bebok-$VERSION-$PLATFORM-portable.tar.gz" -C /tmp/stage "bebok-$VERSION"
  ;;
# ==========================================================================
windows)
  TARGET=x86_64-pc-windows-msvc; PLATFORM=windows-x64; P="$OUT/$PLATFORM"; mkdir -p "$P"
  mkdir -p "$XWIN_CACHE_DIR"
  log "engine: cargo xwin build --release --target $TARGET"
  (cd engine && cargo xwin build --release --locked -p bebok-server --target "$TARGET")
  (cd client && npm run sidecar:copy -- --target "$TARGET" > /dev/null)
  cfg='{}'
  [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ] && cfg='{"bundle":{"createUpdaterArtifacts":true}}'
  echo "$cfg" > client/tauri-release-override.json
  log "tauri build (nsis, cross via cargo-xwin - MSI needs a Windows host)"
  (cd client && npx tauri build --ci --runner cargo-xwin --target "$TARGET" --bundles nsis --config tauri-release-override.json)
  bundle="client/src-tauri/target/$TARGET/release/bundle"; bin="client/src-tauri/target/$TARGET/release"
  app="$bin/bebok-desktop.exe"; [ -f "$app" ] || app="$bin/bebok.exe"
  cp "engine/target/$TARGET/release/bebok-server.exe" "$P/bebok-server-$VERSION-$PLATFORM.exe"
  cp "$bundle"/nsis/*-setup.exe "$P"/
  cp "$bundle"/nsis/*-setup.exe.sig "$P"/ 2>/dev/null || true
  stage="/tmp/stage/bebok-$VERSION"; rm -rf /tmp/stage; mkdir -p "$stage"
  cp "$app" "engine/target/$TARGET/release/bebok-server.exe" LICENSE README.md "$stage"/
  (cd /tmp/stage && zip -qr "$P/bebok-$VERSION-$PLATFORM-portable.zip" "bebok-$VERSION")
  ;;
# ==========================================================================
android)
  PLATFORM=android-arm64; P="$OUT/$PLATFORM"; mkdir -p "$P"
  export BEBOK_ANDROID_ABIS="${BEBOK_ANDROID_ABIS:-arm64-v8a}"
  export BEBOK_SKIP_MKSH="${BEBOK_SKIP_MKSH:-1}"
  log "engine + jniLibs (bundle-android.sh, ABIs: $BEBOK_ANDROID_ABIS)"
  (cd client && bash scripts/bundle-android.sh)
  log "ng build + cap sync"
  (cd client && npm run build > /dev/null && npx cap sync android > /dev/null)
  args=()
  if [ -s /secrets/bebok-android.keystore ] && [ -s /secrets/bebok-android.keystore.password ]; then
    pass="$(tr -d '\n' < /secrets/bebok-android.keystore.password)"
    cp /secrets/bebok-android.keystore /tmp/release.keystore
    args+=("-Pbebok.keystore=/tmp/release.keystore" "-Pbebok.keystorePassword=$pass"
           "-Pbebok.keyAlias=${ANDROID_KEY_ALIAS:-bebok}" "-Pbebok.keyPassword=$pass")
    log "apk signing: on (/secrets/bebok-android.keystore)"
  else
    log "apk signing: OFF - debug key (cannot be upgraded in place by a signed build)"
  fi
  log "gradlew assembleRelease"
  (cd client/android && chmod +x gradlew && ./gradlew --no-daemon -q assembleRelease "${args[@]}")
  apk=$(find client/android/app/build/outputs/apk/release -name '*.apk' | head -1)
  [ -n "$apk" ] || { echo "no APK produced" >&2; exit 1; }
  cp "$apk" "$P/bebok-$VERSION-android-arm64.apk"
  ;;
*)
  echo "unknown BEBOK_BUILD_TARGET=$BEBOK_BUILD_TARGET" >&2; exit 2 ;;
esac

# ---- 5. checksums (same shape as the CI per-platform file) --------------
(cd "$P" && sha256sum -- * > "SHA256SUMS-$PLATFORM.txt")
log "done -> $P"
ls -la "$P"
