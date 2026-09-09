#!/usr/bin/env bash
# Build + bundle the embedded engine (bebok-server) and shell (mksh) for the
# Android app. Run from the `client/` directory:
#
#   npm run android:bundle
#
# Requires: Rust (aarch64-linux-android target) + Android NDK r27 at
# $ANDROID_NDK_HOME (or the default path below). Output lands in
# android/app/src/main/assets/bin/ and is picked up by `npx cap sync`.
set -euo pipefail

CLIENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE_DIR="$CLIENT_DIR/../engine"
NDK_DIR="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/27.2.12479018}"
NDK_BIN="$NDK_DIR/toolchains/llvm/prebuilt/linux-x86_64/bin"
TARGET="aarch64-linux-android"
CLANG="$NDK_BIN/aarch64-linux-android23-clang"

ASSET_BIN="$CLIENT_DIR/android/app/src/main/assets/bin"
mkdir -p "$ASSET_BIN"

if [ ! -x "$CLANG" ]; then
  echo "error: NDK clang not found at $CLANG" >&2
  echo "set ANDROID_NDK_HOME to your NDK root (e.g. .../Sdk/ndk/27.2.12479018)" >&2
  exit 1
fi

# 1. Engine (bebok-server).
echo ">> building engine for $TARGET"
export CC_aarch64_linux_android="$CLANG"
export AR_aarch64_linux_android="$NDK_BIN/llvm-ar"
( cd "$ENGINE_DIR" && cargo build --release --target "$TARGET" )

"$NDK_BIN/llvm-strip" -o "$ASSET_BIN/bebok-server" \
  "$ENGINE_DIR/target/$TARGET/release/bebok-server"
echo "   -> $ASSET_BIN/bebok-server"

# 2. Shell (mksh) - enables the engine's `bash` tool. Best-effort: if it fails,
#    the app still works (everything except the bash tool/terminal).
if [ -x "$ASSET_BIN/mksh" ]; then
  echo ">> mksh already bundled, skipping"
else
  echo ">> building mksh for $TARGET"
  MKSH_SRC="${MKSH_SRC:-$CLIENT_DIR/.mksh-src}"
  if [ ! -f "$MKSH_SRC/Build.sh" ]; then
    git clone --depth 1 https://github.com/MirBSD/mksh.git "$MKSH_SRC" 2>/dev/null || \
      { echo "   (mksh source unavailable; skipping shell - bash tool disabled)"; exit 0; }
  fi
  # Cross-compile with the NDK clang; PATH includes a `clang` symlink so
  # mksh's Build.sh resolves the compiler by name.
  LINK_BIN="$CLIENT_DIR/.ndk-bin"
  mkdir -p "$LINK_BIN"
  ln -sf "$CLANG" "$LINK_BIN/clang"
  ln -sf "$NDK_BIN/llvm-ar" "$LINK_BIN/ar"
  (
    cd "$MKSH_SRC"
    PATH="$LINK_BIN:$PATH" CC=clang TARGET_OS=Android LDSTATIC=1 \
      sh Build.sh -r 2>&1 | tail -5
    cp mksh "$ASSET_BIN/mksh"
  ) || { echo "   (mksh build failed; skipping shell - bash tool disabled)"; }
  echo "   -> $ASSET_BIN/mksh"
fi

echo "done. assets:"
ls -lh "$ASSET_BIN"
