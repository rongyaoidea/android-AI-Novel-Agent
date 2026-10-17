use serde::{Deserialize, Serialize};

/// 一本小说（立项 + 全书走向）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Novel {
    pub id: String,
    pub title: String,
    pub genre: String,
    pub idea: String,
    pub logline: String,
    pub protagonist: String,
    pub style: String,
    pub book_outline: String,
    /// 世界观设定（立项时生成；旧库为空）
    #[serde(default)]
    pub worldview: String,
    /// 立项澄清的问答（JSON 数组：[{question, answer}]；作者已确认的设定，作为立项与写作的硬约束）
    #[serde(default)]
    pub clarify_json: String,
    pub status: String,
    /// "auto"（全自动）| "staged"（按阶段确认）
    pub mode: String,
    /// 文风预设 id（见 styles.rs；空串 = 不限）
    pub style_preset: String,
    /// 全书目标章节数（0 = 未设定）
    #[serde(default)]
    pub target_chapters: i64,
    /// 自动连载：运行完成后自动续写下一批，直到目标达成 / 失败 / 预算上限
    #[serde(default)]
    pub auto_continue: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 角色台账（由每章写后自动抽取，也可手动维护）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Character {
    pub id: String,
    pub novel_id: String,
    pub name: String,
    pub role: String,
    pub status: String,
    pub facts: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 伏笔台账（open：未回收 / resolved：已回收）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Foreshadowing {
    pub id: String,
    pub novel_id: String,
    pub title: String,
    pub status: String,
    pub planted_chapter: i64,
    pub resolved_chapter: Option<i64>,
    pub notes: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 一章（规划 + 正文 + 摘要 + 审校）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub id: String,
    pub novel_id: String,
    pub idx: i64,
    pub title: String,
    pub goal: String,
    pub beats: Vec<String>,
    pub content: String,
    /// 章节摘要（写后自动生成，供后续章节回顾）
    pub summary: String,
    pub status: String,
    pub review_json: Option<String>,
    /// 本地文风检查报告（JSON；空串 = 未检查）
    #[serde(default)]
    pub style_report: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 一次生产运行（可断点续跑）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub novel_id: String,
    pub status: String,
    pub stage: String,
    pub chapter_cursor: i64,
    pub chapters_target: i64,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 模型接入设置（本地保存，不上传）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSettings {
    /// "mock"（离线演示）或 "openai"（任意 OpenAI 兼容接口）
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: f64,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            provider: "mock".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            model: "gpt-4o-mini".into(),
            temperature: 0.8,
        }
    }
}

/// 立项澄清问答（AI 提问 → 作者回答）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyQA {
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub answer: String,
}

/// 一条澄清提问（AI 生成，含可选建议）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyQuestion {
    pub question: String,
    #[serde(default)]
    pub hint: String,
    #[serde(default)]
    pub suggestions: Vec<String>,
}

/// 立项方案（LLM 结构化输出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Brief {
    pub title: String,
    pub genre: String,
    pub logline: String,
    pub protagonist: String,
    pub style: String,
    /// 世界观设定（60 字以内；旧格式输出可以为空）
    #[serde(default)]
    pub worldview: String,
}

/// 章节计划（LLM 结构化输出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterPlan {
    pub idx: i64,
    pub title: String,
    pub goal: String,
    #[serde(default)]
    pub beats: Vec<String>,
}

/// 审校结论
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub ok: bool,
    #[serde(default)]
    pub issues: Vec<ReviewIssue>,
    /// 审校时正文的内容指纹（用于识别「审校之后正文又被改过」的漏审章节；旧数据为空串）
    #[serde(default)]
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewIssue {
    pub severity: String,
    pub note: String,
    #[serde(default)]
    pub quote: String,
    /// 意见状态："open"（仍存在，AI 可再处理）/ "fixed"（AI 已按此修复，旧记录保留供追溯）
    #[serde(default = "default_issue_status")]
    pub status: String,
    /// 本条是否为本轮复核「新发现」（对照上一轮意见后新增的）
    #[serde(default)]
    pub is_new: bool,
    /// 引用句是否已无法在当前正文中定位（正文改过 / 模型引用不逐字）
    #[serde(default)]
    pub stale: bool,
}

pub fn default_issue_status() -> String {
    "open".to_string()
}

/// 引用句能否在当前正文中定位到（忽略空白差异；长引用允许只命中前缀）
pub fn quote_located(content: &str, quote: &str) -> bool {
    let q = quote.trim();
    if q.is_empty() {
        return true; // 没有引用不算失效
    }
    if content.contains(q) {
        return true;
    }
    // 忽略空白差异（草稿保存/换行差异很常见）
    let strip = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    let (cs, qs) = (strip(content), strip(q));
    if cs.contains(&qs) {
        return true;
    }
    // 长引文可能被改写了尾部：从长到短尝试前缀（≥8 字能定位即可）
    let qs_len = qs.chars().count();
    let mut take = qs_len.min(16);
    while take >= 8 {
        let head: String = qs.chars().take(take).collect();
        if cs.contains(&head) {
            return true;
        }
        take -= 1;
    }
    false
}

/// fixed 意见留档上限：超出时保留最近的 N 条（防止 review_json 无限膨胀）。
/// open 意见永不裁剪。
pub const MAX_FIXED_ISSUES: usize = 10;

pub fn cap_review_fixed(review: &mut Review, max_fixed: usize) {
    let fixed_total = review.issues.iter().filter(|i| i.status == "fixed").count();
    if fixed_total <= max_fixed {
        return;
    }
    // 先收集 fixed 的原始下标，保留最后 max_fixed 条
    let fixed_idx: Vec<usize> = review
        .issues
        .iter()
        .enumerate()
        .filter(|(_, i)| i.status == "fixed")
        .map(|(i, _)| i)
        .collect();
    let drop_count = fixed_total - max_fixed;
    let dropped: std::collections::BTreeSet<usize> =
        fixed_idx.into_iter().take(drop_count).collect();
    let mut keep = Vec::with_capacity(review.issues.len());
    for (i, issue) in review.issues.drain(..).enumerate() {
        if !dropped.contains(&i) {
            keep.push(issue);
        }
    }
    review.issues = keep;
}

/// 卷级摘要（每 20 章归并一条，供长书回顾注入）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeSummary {
    pub volume_idx: i64,
    pub upto_chapter: i64,
    pub summary: String,
}

/// 章节历史快照（每次覆盖正文前留档；AI 与人都可据此复原）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterVersion {
    pub id: String,
    pub novel_id: String,
    pub idx: i64,
    /// 内容来源：draft / autofix / rewrite / retry / global_fix / manual / restore / restore-pre / ai-restore
    pub source: String,
    pub status: String,
    pub content: String,
    pub review_json: Option<String>,
    pub style_report: String,
    pub created_at: String,
}

/// 全局（跨章）审校问题
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalIssue {
    pub severity: String,
    pub note: String,
    /// 涉及的章节号（供定向修复）
    #[serde(default)]
    pub affected_chapters: Vec<i64>,
    #[serde(default)]
    pub suggestion: String,
    #[serde(default = "default_issue_status")]
    pub status: String,
    #[serde(default)]
    pub is_new: bool,
}

/// 全局审校结论（整本书维度）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalReview {
    pub ok: bool,
    #[serde(default)]
    pub issues: Vec<GlobalIssue>,
    /// 最近一次检查时间（展示用；解析 LLM 输出时为空）
    #[serde(default)]
    pub updated_at: String,
}

/// 章节列表轻量条目（不含正文，列表接口用；正文走章节详情）
#[derive(Debug, Clone, Serialize)]
pub struct ChapterMeta {
    pub idx: i64,
    pub title: String,
    pub goal: String,
    pub beats: Vec<String>,
    pub status: String,
    pub words: i64,
    pub preview: String,
    pub review_json: Option<String>,
}

/// 书架条目：作品 + 进度统计 + 最近一次运行
#[derive(Debug, Clone, Serialize)]
pub struct NovelListItem {
    pub novel: Novel,
    pub chapters_done: i64,
    pub words: i64,
    pub run_status: Option<String>,
    pub run_stage: Option<String>,
    pub chapters_target: Option<i64>,
    pub last_error: Option<String>,
}

/// 运行记录条目：运行 + 所属作品标题
#[derive(Debug, Clone, Serialize)]
pub struct RunListItem {
    pub run: Run,
    pub novel_title: String,
}

/// 一次 LLM 调用的 token 用量。
/// None = 厂商未提供（不得当作 0）；input/output 为计费口径的输入输出总量，
/// cache_hit / cache_miss 是 input 的两个组成部分（不同厂商字段语义已归一化）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TokenUsage {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_hit: Option<i64>,
    pub cache_miss: Option<i64>,
    pub total: Option<i64>,
}

/// 用量记录（持久化）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub id: String,
    pub novel_id: Option<String>,
    pub chapter_idx: Option<i64>,
    pub task: String,
    pub provider: String,
    pub model: String,
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_hit: Option<i64>,
    pub cache_miss: Option<i64>,
    pub total: Option<i64>,
    pub created_at: String,
}

/// 用量汇总（None = 该口径全部记录都未知，不得显示为 0）
#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageTotals {
    pub calls: i64,
    pub known_input: i64,
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_hit: Option<i64>,
    pub cache_miss: Option<i64>,
    pub total: Option<i64>,
}

/// 用量分组行（key 为任务名或章节号）
#[derive(Debug, Clone, Serialize)]
pub struct UsageGroup {
    pub key: String,
    #[serde(flatten)]
    pub totals: UsageTotals,
}

/// 写作流水线偏好（本地保存，随备份迁移）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineSettings {
    /// 审校出现高/中危问题时自动修复一次（减少人工介入）
    pub auto_fix_review: bool,
    /// 每月 Token 预算上限（0 = 不限）；达到上限后自动连载暂停
    #[serde(default)]
    pub monthly_budget_tokens: i64,
    /// 自动连载每批章节数（1–50，默认 10）
    #[serde(default = "default_auto_batch")]
    pub auto_batch: i64,
    /// 伏笔自动审核间隔（章）：1 = 每批收尾都跑；N = 章节号逢 N 的倍数才跑；0 = 关闭（默认 1）
    #[serde(default = "default_audit_every")]
    pub foreshadow_audit_every: i64,
    /// 全局审校间隔（章）：每写完 N 章做一次全书审校；0 = 关闭（默认 10）
    #[serde(default = "default_global_every")]
    pub global_review_every: i64,
    /// 全局审校发现问题后自动定向修复（默认开）
    #[serde(default = "default_true")]
    pub auto_fix_global: bool,
}

fn default_auto_batch() -> i64 {
    10
}

fn default_audit_every() -> i64 {
    1
}

fn default_global_every() -> i64 {
    10
}

fn default_true() -> bool {
    true
}

impl PipelineSettings {
    pub fn batch(&self) -> i64 {
        self.auto_batch.clamp(1, 50)
    }

    pub fn audit_every(&self) -> i64 {
        self.foreshadow_audit_every.clamp(0, 100)
    }

    pub fn global_every(&self) -> i64 {
        self.global_review_every.clamp(0, 100)
    }
}

impl Default for PipelineSettings {
    fn default() -> Self {
        Self {
            auto_fix_review: true,
            monthly_budget_tokens: 0,
            auto_batch: default_auto_batch(),
            foreshadow_audit_every: default_audit_every(),
            global_review_every: default_global_every(),
            auto_fix_global: true,
        }
    }
}
