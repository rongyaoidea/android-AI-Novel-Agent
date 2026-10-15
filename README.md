# android-AI-Novel-Agent — 本地小说引擎（Rust）+ Android 壳

> 目标：**安卓本地运行**一本 AI 小说创作工作台；**不打包任何 Linux 用户态**（无 Termux、无 node 二进制、无 PRoot/Alpine）。
> 做法：把"引擎"用 **Rust 原生实现**（本仓库自研、范围砍半），UI 用系统 WebView 承载；引擎与 UI 同进程、只监听 `127.0.0.1`。

```
android-AI-Novel-Agent/
├─ engine/          Rust 引擎（lib + CLI，桌面可直接跑；Android 以 cdylib 进 APK）
│  ├─ src/
│  │  ├─ db.rs          SQLite（rusqlite bundled）：novels / chapters / runs / settings
│  │  ├─ llm.rs         LLM 客户端：OpenAI 兼容（含流式 SSE）+ 离线 mock
│  │  ├─ prompts.rs     原创提示词：brief / outline / chapter / review
│  │  ├─ pipeline.rs    生产流水线（可断点续跑）+ 事件总线
│  │  ├─ api.rs         本地 HTTP API + SSE（axum）
│  │  ├─ export.rs      TXT 导出
│  │  └─ jni.rs         Android JNI 入口（--features jni）
│  └─ web/              界面（参考原桌面项目重绘）：书架 / 新建作品 / 本书工作台 / 运行记录 / 模型设置
└─ android/         Android 工程（Kotlin 壳 + 前台服务 + WebView）
```

## 为什么这样选

- **本地运行 + 无 Linux 沙箱** ⇒ 需要把业务逻辑放进"系统自带能力"里：Android 只有三个可用引擎（ART/Kotlin、系统 WebView、Rust `.so`）。
  上游产品是 Node + Prisma 全栈，塞进 WebView 要重写半个运行时；**范围砍半后用 Rust 重写领域层**是成本/可控性最优解。
- Rust 引擎能力：SQLite 存储、OpenAI 兼容模型接入（流式）、章节生产流水线（立项→规划→逐章→审校）、断点续跑、SSE 推送、TXT 导出。
- 交叉编译到 `aarch64-linux-android` 是标准路径（同 typesake 的 Rust core 套路）；APK 内只有自研 `.so` + WebView 资源，几十 MB 量级。

## 已实现（MVP 垂直切片）

| 能力 | 状态 |
|---|---|
| 建项目（一句灵感 → 立项方案 JSON） | ✅ |
| 全书走向 + N 章规划（结构化 JSON） | ✅ |
| 逐章正文生成（流式 token 推送） | ✅ |
| 章节审校（设定冲突/信息超前/角色崩坏/AI 腔，四类问题） | ✅（非阻塞，存为提醒） |
| 断点续跑（已完成章节跳过） | ✅ |
| SSE 实时事件（stage / token / saved / review / finished） | ✅ |
| 导出 TXT | ✅ |
| 模型接入：mock（离线）/ OpenAI 兼容（含 DeepSeek、通义、GLM、Ollama 等） | ✅ |
| 界面：书架卡片 / 开书流程 / 工作台（进度步骤条 + 跟随生成 + 章节表）/ 运行记录 / 模型设置（浅色 & 深色主题） | ✅ |
| 角色台账 / 伏笔台账（写后自动抽取 + 上下文注入） | ✅ |
| 选段 AI 修文（改写/扩写/精简/去 AI 味，diff 预览 + 换一版） | ✅ |
| 写作工具：光标续写 / 章节重写 / 摘要链（近 5 章注入） / 书籍体检 | ✅ |
| 文风预设（10 套，蒸馏自番茄生态调研：悬疑限知 / 热血群像 / 系统神豪 / 玄幻打脸 / 苟道种田 / 纪实江湖 / 发疯毒舌 / 无 CP 大女主 / 拉扯甜虐 / 古风权谋），建书时选择、工作台可随时更换 | ✅ |
| 数据安全：全量备份导出 & 恢复（zip 快照）、本地服务访问令牌、API Key 落库加密（Android Keystore 包裹密钥 + AES-256-GCM） | ✅ |
| AI 用量统计：按任务/章节记账，输入 / 输出 / 缓存命中 / 未命中（多厂商字段已归一化，未知不按 0 计） | ✅ |
| Android 壳（前台服务 + WebView + JNI，下载/选文件已接系统） | ✅ 脚手架（见 `android/README.md` 构建步骤） |
| 阅测/EPUB/多卷结构 | ⏳ 路线图 |

## 桌面运行（开发/验证）

```bash
cd engine
cargo run                                   # 默认 http://127.0.0.1:8787，数据在 ./data
cargo run -- --port 8899 --data-dir ./devdata
```

打开 `http://127.0.0.1:8787/`：
1. 「模型接入」选 `mock` 可**零配置离线**跑通全流程（推荐先用它验证）；
2. 或选 `openai`，填 Base URL / Key / 模型（如 `https://api.deepseek.com/v1` + `deepseek-chat`）；
3. 「新建小说」写一句灵感 → 开始创作 → 看实时流与章节列表；导出 TXT。

测试（含端到端 mock 流水线）：

```bash
cd engine
cargo test
```

## 数据安全

- **备份 / 恢复**：模型设置页「数据备份」→「导出备份」下载 `ainovel-backup-*.zip`（内含 `VACUUM INTO` 一致性快照），「恢复备份」选 zip 覆盖当前数据（128 MB 上限，恢复前二次确认）。Android 上导出文件保存到系统「下载」。
- **本地服务令牌**：`/api/*` 需要 `x-ainovel-token` 头或 `?token=`；`/api/health` 与静态资源放行。Android 壳首次启动生成 40 位 hex 令牌（私有 SharedPreferences），注入 WebView 与通知轮询。桌面开发默认不设防；需要时可设：

```bash
# PowerShell
$env:AI_NOVEL_AUTH_TOKEN = "your-token"; cargo run
```

- **API Key 加密**：提供 64 位 hex 密钥后，Key 以 `enc:v1:` 前缀的 AES-256-GCM 密文落库（换密钥后解不开会被置空并提示重填）：

```bash
# PowerShell（桌面开发用随机密钥即可；Android 上由 Keystore 包裹的密钥自动提供）
$env:AI_NOVEL_SECRET_HEX = "abababababababababababababababababababababababababababababababab"; cargo run
```

- **AI 用量**：工作台「AI 用量」弹窗查看：调用次数、输入 / 输出、缓存命中与未命中（OpenAI `prompt_tokens_details.cached_tokens`、DeepSeek `prompt_cache_hit/miss_tokens`、Anthropic `cache_read/creation_input_tokens` 已归一化），按任务与章节分组；厂商未上报的口径显示「—」而不是 0。

## 文风预设（蒸馏自平台生态调研）

调研与蒸馏过程见 [`docs/风格调研-番茄.md`](docs/风格调研-番茄.md)；规则数据在 `engine/src/styles.rs`。

- **10 套预设**：悬疑限知·反转烧脑 / 热血高武·少年群像 / 系统神豪·都市短爽 / 玄幻打脸·废材升级 / 苟道修仙·日常种田 / 第一人称·纪实江湖 / 发疯毒舌·金句怼人 / 无 CP 大女主·独狼升级 / 现代言情·拉扯甜虐 / 古风雅致·家宅权谋。
- **选择方式**：新建作品页按「全部 / 男频 / 女频 / 通用」筛选并点选（选择记忆在本地）；工作台标题旁的「文风：…」chip 可随时更换（`PATCH /api/novels/:id`）。
- **注入方式**：选中后，平台节奏基线（单章 2000 字级、章末三行必须留钩、段落长短交错禁匀速等）＋ 风格规则 ＋ 反例清单 ＋ 开场/章末参考，会追加进立项 / 章节规划 / 正文 / 审校 / 修文 / 续写 / 重写的提示词；未选预设保持原有默认写法。
- **目录接口**：`GET /api/styles`（含规则与反例，供前端展示与自检复用）。

## Android 构建（概要）

```bash
rustup target add aarch64-linux-android
cargo install cargo-ndk
cd engine
cargo ndk -t arm64-v8a -o ../android/app/src/main/jniLibs build --release --features jni

cd ../android
# 需要 local.properties 指向 Android SDK（sdk.dir=...），NDK 建议 26.x+；本机需有 gradle（或用 Android Studio 打开）
gradle assembleDebug               # 产物 app/build/outputs/apk/debug/app-debug.apk
```

细节与常见问题见 `android/README.md`。

## 独立实现与范围说明（clean-room）

- 本仓库为**独立实现**：不复制任何既有项目的代码；提示词为本项目原创精简版，仅参考通用产品思路。
- 范围（"砍半"）：
  - **保留**：开书方向、全书走向、章节规划、逐章正文、章节审校、断点续跑、阅读、导出、模型配置；
  - **砍掉（近期不做）**：拆书、知识库/RAG、漫画/短剧、世界样本库、旧版导演、图片生成、PostgreSQL、多用户。
- 数据互通：如需与其它系统交换数据，可另做 SQLite schema 映射层（纯 SQL 迁移可直接执行）。

## 路线图

- **P0（本仓库现状）**：垂直切片跑通（桌面 + mock + 真模型）。
- **P1**：Android 真机跑通；通知（完成/失败）；章节继续写与重写；提示词工程化（模板文件化、按书覆盖）。
- **P2**：角色/伏笔最小台账；阅读器体验；EPUB 导出；模型路由（规划/正文/审校分模型）。
- **P3**：多卷结构；RAG（本地向量，可选）；单书导入导出（整库备份/恢复已提供）。

## 发版与签名（GitHub Actions）

- CI：`.github/workflows/android.yml`（手动触发 / 推 `v*` 标签 / 发布 Release）。
- 签名：keystore 以 Base64 存在仓库 Secrets，**四个密钥长期复用同一套**：
  `RELEASE_KEYSTORE_B64` / `RELEASE_STORE_PASSWORD` / `RELEASE_KEY_ALIAS` / `RELEASE_KEY_PASSWORD`。
- 发版流程：更新 `android/app/build.gradle.kts` 的 `versionCode/versionName` → 打标签 `vX.Y.Z` 并推送 → CI 产出签名 APK → 创建 Release 后 APK 自动附为附件。
- 本地无 keystore 时自动回退 debug 签名（仅自测用，不可用于覆盖发布版升级）。

## 许可

MIT（本仓库自研代码）。接入的模型服务与第三方库遵循各自许可。
