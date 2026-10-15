# Android 壳（Kotlin）

结构：

- `app/src/main/java/com/ainovel/engine/MainActivity.kt` — WebView 容器；启动服务 → 轮询 `127.0.0.1:8787` 就绪 → 加载本地 UI。
- `app/src/main/java/com/ainovel/engine/EngineService.kt` — 前台服务；`System.loadLibrary("ainovel_engine")` + `nativeStart(filesDir, 8787)`。
- `app/src/main/jniLibs/arm64-v8a/` — Rust 引擎产物（构建生成，不入库）。

## 本地构建

前置：

```bash
rustup target add aarch64-linux-android
cargo install cargo-ndk          # 需要本地已装 Android NDK（建议 26.x+）
```

1) 编译引擎为 Android 动态库（在 `engine/` 目录）：

```bash
cargo ndk -t arm64-v8a -o ../android/app/src/main/jniLibs build --release --features jni
```

产物：`android/app/src/main/jniLibs/arm64-v8a/libainovel_engine.so`

2) 配置 SDK 路径（`android/local.properties`，不入库）：

```properties
sdk.dir=C:\\Users\\<你>\\AppData\\Local\\Android\\Sdk
```

3) 构建 APK（本机需有 gradle，或用 Android Studio 打开本目录）：

```bash
cd android
gradle assembleDebug               # 调试包
gradle assembleRelease             # 发布包（无 keystore 时自动回退 debug 签名）
```

产物：`android/app/build/outputs/apk/...`

## 发版与签名（GitHub Actions）

- 工作流：`.github/workflows/android.yml`（手动 / 推 `v*` 标签 / 发布 Release）。
- 签名密钥固定在仓库 Secrets（**长期复用同一套 keystore，保证覆盖升级**）：
  `RELEASE_KEYSTORE_B64`、`RELEASE_STORE_PASSWORD`、`RELEASE_KEY_ALIAS`、`RELEASE_KEY_PASSWORD`。
- 发版步骤：改 `app/build.gradle.kts` 的 `versionCode/versionName` → `git tag vX.Y.Z && git push --tags` → 去 GitHub 创建 Release（基于该标签）→ APK 自动作为附件上传。
- 本地测试：把 keystore 放到 `android/release.keystore` 即走真实签名（文件已被 .gitignore 排除）。

## 设计要点

- **零外部运行时**：APK 内没有 Node/Termux/PRoot；引擎是单个自研 `.so`。
- **仅回环**：引擎只绑定 `127.0.0.1`；`usesCleartextTraffic` 仅为回环服务（后续可换 `networkSecurityConfig` 白名单收紧）。
- **前台服务**：`dataSync` 类型；生成任务在服务进程内跑，退出界面不中断。
- **数据目录**：`filesDir/ainovel.db`（SQLite，覆盖安装保留）。
- **手机适配**：Web UI 在窄屏下自动切换为顶部导航 + 单列布局；表格横向滚动。

## 已知事项（下一步）

- [ ] 通知：生成完成/失败的通知（服务内监听引擎事件 → 本地通知）。
- [ ] `networkSecurityConfig` 收紧 cleartext（仅 127.0.0.1）。
- [ ] 图标为矢量占位，正式版替换。
- [ ] 若要在 x86_64 模拟器跑：`rustup target add x86_64-linux-android` 并按 ABI 追加编译。
