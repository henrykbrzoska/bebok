#!/usr/bin/env bash
# Build + bundle the embedded engine (bebok-server) and shell (mksh) for the
# Android app, for one or more ABIs. Run from the `client/` directory:
#
#   npm run android:bundle
#   BEBOK_ANDROID_ABIS="arm64-v8a x86_64" npm run android:bundle
#
# Requires: Rust with the matching `*-linux-android` targets installed
# (`rustup target add aarch64-linux-android x86_64-linux-android`) + Android
# NDK r27 at $ANDROID_NDK_HOME (or the default path below).
#
# F10-29: the binaries are packaged as *native libraries* -
# android/app/src/main/jniLibs/<abi>/libbebok_server.so (+ libmksh.so) -
# not as assets. Android 10+ (targetSdk >= 29) refuses to exec anything the
# app itself wrote into its data dir (W^X for untrusted_app: "error=13,
# Permission denied" on the S25 Ultra / Android 16), while files the
# *installer* placed in ApplicationInfo.nativeLibraryDir (lib/<abi>/lib*.so,
# extracted because build.gradle sets jniLibs.useLegacyPackaging) may be
# exec'd. They are ordinary PIE executables that merely carry a lib*.so
# name; EngineLauncherPlugin runs them straight from nativeLibraryDir. The
# ABI choice is the installer's (build.gradle limits the APK to the ABIs
# present here via abiFilters), so no per-ABI asset lookup exists any more.
#
# BEBOK_ANDROID_ABIS: space-separated list of Android ABI names to build for.
# Default: "arm64-v8a" (real devices). CI additionally builds "x86_64" for the
# emulator. Supported: arm64-v8a, x86_64.
#
# POSIX-sh compatible (no bash-only array features beyond ${BASH_SOURCE[0]},
# which every bash - including Git Bash on Windows - provides): this script
# runs on Ubuntu CI and on Windows via Git Bash/WSL.
set -euo pipefail

CLIENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE_DIR="$CLIENT_DIR/../engine"
JNI_LIBS="$CLIENT_DIR/android/app/src/main/jniLibs"
LEGACY_ASSET_BIN="$CLIENT_DIR/android/app/src/main/assets/bin"
SERVER_LIB="libbebok_server.so"
SHELL_LIB="libmksh.so"

ABIS="${BEBOK_ANDROID_ABIS:-arm64-v8a}"

# Map an Android ABI name to its Rust target triple and NDK API-level clang.
abi_target() {
  case "$1" in
    arm64-v8a) echo "aarch64-linux-android" ;;
    x86_64) echo "x86_64-linux-android" ;;
    *) echo "" ;;
  esac
}

# Detect the NDK host-tag subdirectory under toolchains/llvm/prebuilt/. Do NOT
# hard-code `linux-x86_64` - CI runs on Ubuntu, but this script must also work
# on a Windows or macOS workstation.
ndk_host_tag() {
  case "$(uname -s)" in
    Linux*) echo "linux-x86_64" ;;
    Darwin*) echo "darwin-x86_64" ;;
    MINGW*|MSYS*|CYGWIN*) echo "windows-x86_64" ;;
    *) echo "linux-x86_64" ;;
  esac
}

NDK_DIR="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-$HOME/Android/Sdk/ndk/27.2.12479018}}"
if [ ! -d "$NDK_DIR" ]; then
  echo "error: NDK not found at $NDK_DIR" >&2
  echo "set ANDROID_NDK_HOME to your NDK r27 root (e.g. .../Sdk/ndk/27.2.12479018)" >&2
  exit 1
fi
HOST_TAG="$(ndk_host_tag)"
NDK_BIN="$NDK_DIR/toolchains/llvm/prebuilt/$HOST_TAG/bin"
NDK_API=23

mkdir -p "$JNI_LIBS"
# Pre-F10-29 layout: a stale assets/bin/ would still be packaged (never used)
# and double the APK size - drop it.
if [ -d "$LEGACY_ASSET_BIN" ]; then
  echo ">> removing legacy $LEGACY_ASSET_BIN (the engine now ships in jniLibs/)"
  rm -rf "$LEGACY_ASSET_BIN"
fi

echo ">> ABIs: $ABIS (NDK host tag: $HOST_TAG)"

# Track stripped binary sizes for the report/CI log.
SIZE_REPORT=""

for ABI in $ABIS; do
  TARGET="$(abi_target "$ABI")"
  if [ -z "$TARGET" ]; then
    echo "error: unknown ABI '$ABI' (supported: arm64-v8a, x86_64)" >&2
    exit 1
  fi

  CLANG="$NDK_BIN/${TARGET}${NDK_API}-clang"
  CLANGXX="$NDK_BIN/${TARGET}${NDK_API}-clang++"
  if [ "$HOST_TAG" = "windows-x86_64" ]; then
    # On Windows the extension-less `<target><api>-clang` is a *bash* script
    # (it execs clang.exe with `--target=...`); a natively-run rustc/cc-rs
    # process cannot exec a shebang script directly (CreateProcess fails with
    # "not a valid Win32 application"). The `.cmd` sibling is a real batch
    # wrapper Windows can spawn - always prefer it here, regardless of the
    # POSIX executable bit on the extension-less file (Git Bash sets that bit
    # on both, so `-x` cannot tell them apart).
    CLANG="$CLANG.cmd"
    CLANGXX="$CLANGXX.cmd"
  fi
  # `-x` is unreliable for .cmd/.exe on Windows (NTFS has no POSIX exec bit;
  # Git Bash infers it inconsistently), so just check existence there.
  if [ "$HOST_TAG" = "windows-x86_64" ]; then
    CLANG_OK=$([ -f "$CLANG" ] && echo 1 || echo "")
  else
    CLANG_OK=$([ -x "$CLANG" ] && echo 1 || echo "")
  fi
  if [ -z "$CLANG_OK" ]; then
    echo "error: NDK clang not found at $CLANG" >&2
    echo "set ANDROID_NDK_HOME to your NDK r27 root (e.g. .../Sdk/ndk/27.2.12479018)" >&2
    exit 1
  fi

  AR="$NDK_BIN/llvm-ar"
  STRIP="$NDK_BIN/llvm-strip"
  if [ "$HOST_TAG" = "windows-x86_64" ]; then
    AR="$AR.exe"
    STRIP="$STRIP.exe"
  fi
  TARGET_UPPER="$(echo "$TARGET" | tr '[:lower:]-' '[:upper:]_')"

  ABI_LIB_DIR="$JNI_LIBS/$ABI"
  mkdir -p "$ABI_LIB_DIR"

  echo ">> building engine for $ABI ($TARGET)"
  export CC_"${TARGET//-/_}"="$CLANG"
  export CXX_"${TARGET//-/_}"="$CLANGXX"
  export AR_"${TARGET//-/_}"="$AR"
  export "CARGO_TARGET_${TARGET_UPPER}_LINKER"="$CLANG"
  ( cd "$ENGINE_DIR" && cargo build --release --target "$TARGET" -p bebok-server )

  "$STRIP" -o "$ABI_LIB_DIR/$SERVER_LIB" \
    "$ENGINE_DIR/target/$TARGET/release/bebok-server"
  SIZE=$(wc -c < "$ABI_LIB_DIR/$SERVER_LIB" | tr -d ' ')
  SIZE_HUMAN=$(du -h "$ABI_LIB_DIR/$SERVER_LIB" | awk '{print $1}')
  echo "   -> $ABI_LIB_DIR/$SERVER_LIB ($SIZE_HUMAN, $SIZE bytes)"
  SIZE_REPORT="$SIZE_REPORT
$ABI/$SERVER_LIB: $SIZE_HUMAN ($SIZE bytes)"

  # mksh (bash tool / terminal) for this ABI - best-effort, same semantics as
  # before: if it fails, the app still works minus the bash tool.
  if [ -s "$ABI_LIB_DIR/$SHELL_LIB" ]; then
    echo ">> mksh already bundled for $ABI, skipping"
  else
    echo ">> building mksh for $ABI ($TARGET)"
    MKSH_SRC="${MKSH_SRC:-$CLIENT_DIR/.mksh-src}"
    if [ ! -f "$MKSH_SRC/Build.sh" ]; then
      git clone --depth 1 https://github.com/MirBSD/mksh.git "$MKSH_SRC" 2>/dev/null || \
        echo "   (mksh source unavailable; skipping shell for $ABI - bash tool disabled)"
    fi
    if [ -f "$MKSH_SRC/Build.sh" ]; then
      # Cross-compile with the NDK clang; PATH includes a `clang`/`ar` symlink
      # (per-ABI, to avoid clobbering a concurrent/previous ABI's build) so
      # mksh's Build.sh resolves the compiler by name.
      LINK_BIN="$CLIENT_DIR/.ndk-bin-$ABI"
      mkdir -p "$LINK_BIN"
      ln -sf "$CLANG" "$LINK_BIN/clang"
      ln -sf "$AR" "$LINK_BIN/ar"
      (
        cd "$MKSH_SRC"
        # mksh's Build.sh does not support out-of-tree builds; `make clean`
        # equivalent between ABIs so stale objects from a previous target
        # are not relinked into this one.
        rm -f mksh mksh.exe *.o 2>/dev/null || true
        PATH="$LINK_BIN:$PATH" CC=clang TARGET_OS=Android LDSTATIC=1 \
          sh Build.sh -r 2>&1 | tail -5
        cp mksh "$ABI_LIB_DIR/$SHELL_LIB"
      ) || echo "   (mksh build failed for $ABI; skipping shell - bash tool disabled)"
      if [ -f "$ABI_LIB_DIR/$SHELL_LIB" ]; then
        echo "   -> $ABI_LIB_DIR/$SHELL_LIB"
      fi
    fi
  fi
done

echo
echo "done. native libs (jniLibs/<abi>/):"
find "$JNI_LIBS" -type f -exec ls -lh {} \;
echo
echo "== stripped engine size per ABI =="
echo "$SIZE_REPORT"
