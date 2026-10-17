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
| 模型接入：mock（离线）/ OpenAI 兼容（内置 14 家快速预设：DeepSeek、通义、GLM、Kimi、硅基流动、OpenAI、OpenRouter、xAI Grok、Groq、OpenCode Zen、OpenCode Go、Command Code、Ollama、LM Studio）；Base URL 缺 /v1 自动补全回退；支持一键拉取服务商模型列表 | ✅ |
| 界面：书架卡片 / 开书流程 / 工作台（进度步骤条 + 跟随生成 + 章节表）/ 运行记录 / 模型设置（浅色 & 深色主题） | ✅ |
| 书架搜索与排序（最近更新 / 字数最多 / 进度最高）；运行日志刷新后回放（每书最近 300 条关键事件） | ✅ |
| 角色台账 / 伏笔台账（写后自动抽取 + 上下文注入） | ✅ |
| 选段 AI 修文（改写/扩写/精简/去 AI 味，diff 预览 + 换一版） | ✅ |
| 写作工具：光标续写 / 章节重写 / 摘要链（近 5 章注入） / 书籍体检 | ✅ |
| 文风检查器（本地、零 token）：段落长短/对话占比/句长/禁用短语/章末钩子/字数窗口逐项检查，硬伤自动重写一次，结果并入审校意见 | ✅ |
| 全阶段实时进度：立项 / 规划 / 正文 / 审校 / 自动修复 / 资料整理都有状态栏跟随（含秒表与阶段高亮），不再只有正文可跟随 | ✅ |
| 审校自动修复（可开关，默认开）：高/中危意见自动修订并复核，仅在问题变少时采用；伏笔注入带「已拖延章数」，回收自动标记 —— 尽量无需人工介入 | ✅ |
| **双层审校**：章节级（逐章意见/修复）+ **全局审校**（全书视角检查跨章一致性 / 节奏 / 支线，每 N 章自动 + 随时手动）；全局问题自动定向修复受影响章节，修完复检、状态自动归并 | ✅ |
| **章节版本历史 + AI 复原**：每次覆盖正文前自动留档（最近 10 版，含当时的审校与文风报告）；编辑器「历史」里可预览/回滚；「AI 复原」按本地评分（文风分 - 未解决意见 - 篇幅塌陷）自动选最优版本恢复，修复改差时也会自愈 | ✅ |
| 审校意见状态化：每条意见有生命周期（仍存在 / 已修复 / 新发现）；复核按语义归并，同一问题换个说法不会变成两条；引用对不上正文时明确标注并自动重定位 | ✅ |
| 伏笔自动审核回收：每批连载收尾与「一键处理」时，用章节摘要批量判定未回收伏笔是否已被后文实质回收，自动标记（有依据才回收，不误标）；频率可配（每批 / 每 N 章 / 关闭） | ✅ |
| 写前事实预检（本地、零 token）：生成前用台账核对章纲——已死亡/退场角色被提到、与近 5 章目标高度重复等矛盾点，随提示词注入提醒模型避开 | ✅ |
| 卷级记忆：每 20 章自动归并一条卷级摘要，生成新章时注入「最近 3 卷摘要 + 最近 5 章摘要」，长书不再断层 | ✅ |
| 全文检索（FTS5）：人名 / 道具 / 台词一键搜索到章，中文逐字索引支持包含式检索，点结果直接打开章节 | ✅ |
| 作品总纲：书架卡片「总纲」一键查看世界观 / 全书走向 / 卖点 / 主角 / 基调与进度 | ✅ |
| 审校呈机关：「AI 处理全部审校」一键让 Agent 重跑所有待处理章节；**章节表如实显示每条意见的解决状态**——有未解决意见显示「余留 N 条」（点击直达编辑器），全部解决才显示「AI 已处理」；概览显示「已覆盖 N 章 · 余留 M 条」 | ✅ |
| **开工巡检**：每批运行开始自动做（零成本运维）：done 却无正文的章节收回重写、缺摘要自动补齐、**正文在审校之后被改动的章节自动重审**（手动编辑/AI 单章操作不再漏审）；书籍体检里可手动全量巡检 | ✅ |
| **自动连载（agent 模式）**：设全书目标章节数，一键后按 ≤10 章/批自动续写直到达标；批次间有运行检查点（崩溃可续）；中途可随时停止 | ✅ |
| **停止创作**：一键停止进行中的生成（流式输出随断、已写章节全部保留），自动连载同步关闭；点「继续创作」可断点续写 | ✅ |
| **Token 预算护栏**：设每月上限，连载/运行到上限自动暂停并提示；用量弹窗显示本月累计与预算 | ✅ |
| 章节级自愈：正文空内容 / 立项与规划 JSON 解析失败自动重试一次；网络/限流/5xx 退避重试 | ✅ |
| LLM 调用容错：网络错误 / 408 / 429 / 5xx 自动退避重试（最多 3 次） | ✅ |
| 文风预设（10 套，蒸馏自番茄生态调研：悬疑限知 / 热血群像 / 系统神豪 / 玄幻打脸 / 苟道种田 / 纪实江湖 / 发疯毒舌 / 无 CP 大女主 / 拉扯甜虐 / 古风权谋），建书时选择、工作台可随时更换 | ✅ |
| 提示词可编辑：11 个生成任务的 system / user 模板全部可改，`{{变量}}` 占位符校验 + 变量一键插入 + 恢复默认，改动即时生效 | ✅ |
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

## 提示词可编辑

在「模型设置 → 提示词模板」中可修改 11 个任务的提示词（立项 / 章节规划 / 正文 / 审校 / 选段修文 / 续写 / 重写 / 资料整理 / 卷级摘要 / 伏笔审核 / 全局审校）：

- **占位符**：`{{变量名}}` 会被替换为实际内容（如 `{{idx}}`、`{{beats}}`、`{{style_block}}`）；界面上一键点击即可插入，保存时校验变量名，非法直接 400。
- **兼容兜底**：自定义模板若删掉 `TASK:xxx` 标记，渲染时会自动补一行，保证离线 Mock（演示模式）与流水线不受影响。
- **生效范围**：保存后立即对后续生成生效；「恢复默认」一键还原（覆盖存储在本地 settings 表，随备份一起迁移）。
- **接口**：`GET /api/prompts`（目录 + 默认全文 + 当前覆盖）、`PUT /api/prompts/:task`、`DELETE /api/prompts/:task`。

## 文档

- 风格调研与预设映射：[`docs/风格调研-番茄.md`](docs/风格调研-番茄.md)
- 生产就绪评估（分场景结论 + 投产前清单）：[`docs/生产就绪评估.md`](docs/生产就绪评估.md)

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
