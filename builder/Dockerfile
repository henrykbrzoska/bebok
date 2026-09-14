# syntax=docker/dockerfile:1.7
# Bebok builder images - the same toolchains the release workflow uses, so a
# local build produces the same artifacts as CI (ubuntu-22.04 base like the
# linux-x64 leg). Three targets, one per compose service:
#
#   linux    deb + AppImage + portable tar.gz          (x86_64-unknown-linux-gnu)
#   windows  NSIS installer + portable zip (cargo-xwin) (x86_64-pc-windows-msvc, MSI needs Windows)
#   android  signed APK (arm64-v8a)                     (aarch64-linux-android)
#
# macOS cannot be built in a container; `npm run builder` runs that leg on a
# macOS host natively.

ARG RUST_VERSION=1.97.0
ARG NODE_MAJOR=22

# --------------------------------------------------------------------------
FROM ubuntu:22.04 AS base
ARG RUST_VERSION
ARG NODE_MAJOR
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    CARGO_TERM_COLOR=always \
    CARGO_INCREMENTAL=0
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git rsync xz-utils unzip zip pkg-config \
      build-essential file libssl-dev python3 jq \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://deb.nodesource.com/setup_${NODE_MAJOR}.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --no-modify-path --default-toolchain ${RUST_VERSION} --profile minimal \
    && rustup component add rustfmt clippy
COPY builder/entrypoint.sh /usr/local/bin/bebok-build
RUN chmod +x /usr/local/bin/bebok-build
WORKDIR /work
ENTRYPOINT ["/usr/local/bin/bebok-build"]

# --------------------------------------------------------------------------
FROM base AS linux
# Same list as release.yml "Linux system dependencies (Tauri 2)".
RUN apt-get update && apt-get install -y --no-install-recommends \
      libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev \
      libgtk-3-dev libxdo-dev libfuse2 patchelf desktop-file-utils \
    && rm -rf /var/lib/apt/lists/*
ENV BEBOK_BUILD_TARGET=linux

# --------------------------------------------------------------------------
FROM base AS windows
# Tauri's documented Linux -> Windows path: cargo-xwin (pulls the Windows SDK
# + CRT under the xwin cache), lld/clang as the linker, NSIS from apt.
RUN apt-get update && apt-get install -y --no-install-recommends \
      nsis lld llvm clang \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-pc-windows-msvc \
    && cargo install cargo-xwin --locked
ENV BEBOK_BUILD_TARGET=windows XWIN_ARCH=x86_64 XWIN_CACHE_DIR=/cache/xwin

# --------------------------------------------------------------------------
FROM base AS android
ARG NDK_VERSION=27.2.12479018
ARG ANDROID_PLATFORM=android-35
ARG BUILD_TOOLS=35.0.0
ARG CMDLINE_TOOLS=11076708
ENV ANDROID_HOME=/opt/android-sdk \
    ANDROID_SDK_ROOT=/opt/android-sdk \
    ANDROID_NDK_HOME=/opt/android-sdk/ndk/${NDK_VERSION} \
    JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64
RUN apt-get update && apt-get install -y --no-install-recommends openjdk-21-jdk-headless \
    && rm -rf /var/lib/apt/lists/*
RUN mkdir -p ${ANDROID_HOME}/cmdline-tools \
    && curl -fsSL -o /tmp/ct.zip https://dl.google.com/android/repository/commandlinetools-linux-${CMDLINE_TOOLS}_latest.zip \
    && unzip -q /tmp/ct.zip -d ${ANDROID_HOME}/cmdline-tools \
    && mv ${ANDROID_HOME}/cmdline-tools/cmdline-tools ${ANDROID_HOME}/cmdline-tools/latest \
    && rm /tmp/ct.zip \
    && yes | ${ANDROID_HOME}/cmdline-tools/latest/bin/sdkmanager --licenses > /dev/null \
    && ${ANDROID_HOME}/cmdline-tools/latest/bin/sdkmanager --install \
         "platform-tools" "platforms;${ANDROID_PLATFORM}" "build-tools;${BUILD_TOOLS}" "ndk;${NDK_VERSION}" > /dev/null
RUN rustup target add aarch64-linux-android x86_64-linux-android
ENV BEBOK_BUILD_TARGET=android PATH=/opt/android-sdk/platform-tools:$PATH
