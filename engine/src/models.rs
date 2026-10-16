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

/// 立项方案（LLM 结构化输出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Brief {
    pub title: String,
    pub genre: String,
    pub logline: String,
    pub protagonist: String,
    pub style: String,
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
}

pub fn default_issue_status() -> String {
    "open".to_string()
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
}

fn default_auto_batch() -> i64 {
    10
}

impl PipelineSettings {
    pub fn batch(&self) -> i64 {
        self.auto_batch.clamp(1, 50)
    }
}

impl Default for PipelineSettings {
    fn default() -> Self {
        Self {
            auto_fix_review: true,
            monthly_budget_tokens: 0,
            auto_batch: default_auto_batch(),
        }
    }
}
