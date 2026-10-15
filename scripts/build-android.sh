#!/usr/bin/env bash
# 一键构建：Rust 引擎（arm64-v8a）→ Android APK
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if ! command -v cargo-ndk >/dev/null 2>&1; then
  echo "未找到 cargo-ndk：请先执行 cargo install cargo-ndk（并确保已安装 Android NDK）" >&2
  exit 1
fi
if ! rustup target list --installed | grep -q aarch64-linux-android; then
  rustup target add aarch64-linux-android
fi

echo "[1/2] 编译 Rust 引擎 (aarch64-linux-android) ..."
cd "$ROOT/engine"
cargo ndk -t arm64-v8a -o "$ROOT/android/app/src/main/jniLibs" build --release --features jni

echo "[2/2] 构建 APK ..."
cd "$ROOT/android"
if [ ! -f local.properties ]; then
  echo "提示：需要 android/local.properties（sdk.dir=...），参见 android/README.md" >&2
fi
./gradlew assembleDebug

echo "完成：android/app/build/outputs/apk/debug/app-debug.apk"
