# Android 壳（Kotlin）

结构：

- `app/src/main/java/com/ainovel/engine/MainActivity.kt` — WebView 容器；启动服务 → 轮询 `127.0.0.1:8787` 就绪 → 带令牌加载本地 UI；接管下载（导出 TXT / 备份 zip → 系统「下载」）与文件选择（恢复备份）。
- `app/src/main/java/com/ainovel/engine/EngineService.kt` — 前台服务；`System.loadLibrary("ainovel_engine")` + `nativeStart(filesDir, 8787, token, secretHex)`；轮询运行状态推「完成 / 中断 / 待确认」通知。
- `app/src/main/java/com/ainovel/engine/SecretStore.kt` — 本机密钥：Android Keystore 主密钥包裹 32 字节数据密钥（供引擎加密 API Key）；生成并持久化本地服务访问令牌。
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
- **仅回环 + 访问令牌**：引擎只绑定 `127.0.0.1`，且 `/api/*` 需要 `x-ainovel-token`（壳生成的 40 位 hex，存私有 SharedPreferences），防止同机其它 App 通过 8787 读取作品与设置；`/api/health` 与静态资源放行。WebView 首次加载 URL 带 `?token=`，页面 JS 暂存后自动从地址栏清除。
- **敏感数据落库加密**：API Key 以 AES-256-GCM 密文存储；数据密钥随机生成、由 Android Keystore 主密钥（不可导出）包裹后持久化，卸载即失效。
- **前台服务**：`dataSync` 类型；生成任务在服务进程内跑，退出界面不中断。
- **数据目录**：`filesDir/ainovel.db`（SQLite，覆盖安装保留）。
- **手机适配**：Web UI 在窄屏下自动切换为顶部导航（sticky）+ 单列布局；表格横向滚动、导航横滑条隐藏。
- **系统栏适配**：Android 15（targetSdk 35）强制 edge-to-edge，`MainActivity` 把系统栏 / 输入法 insets 转为 WebView 内边距，避免顶部导航被状态栏压住点不到；窗口底色取 Web UI 的「纸」色，状态栏区域自然过渡。
- **应用图标**：自适应图标（白底 + 手绘墨线书 + 朱砂印 + 一笔墨扫），含 Android 13+ 单色（主题图标）版本。
- **下载/上传**：WebView 默认不处理下载与文件选择，壳已补 `DownloadListener`（MediaStore → 系统「下载」）与 `onShowFileChooser`（ACTION_GET_CONTENT，用于选备份 zip）。

## 已知事项（下一步）

- [ ] `networkSecurityConfig` 收紧 cleartext（仅 127.0.0.1）。
- [ ] 图标为矢量占位，正式版替换。
- [ ] 若要在 x86_64 模拟器跑：`rustup target add x86_64-linux-android` 并按 ABI 追加编译。
