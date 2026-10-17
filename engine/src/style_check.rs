//! 本地文风一致性检查（零 token 成本）：把文风预设里的量化规则变成真实检查。
//!
//! 检查项（与 `styles.rs` 的规则一一对应）：
//! - 段落长短交错（变异系数 CV；全段等长 = AI 味头号特征，硬失败）
//! - 对话占比（部分预设要求 ≥ 35%~45%）
//! - 单句最长（沙雕风要求 ≤ 45 字等）
//! - 禁用短语（全局 AI 味黑名单 + 古风/纪实禁现代词）
//! - 章末悬念（启发式，软失败）
//! - 单章字数窗口（解析预设的 word_target）
//!
//! 判定分两档：`passed`（是否满足）与 `hard`（不满足时是否算硬伤）。
//! 硬伤会触发流水线的自动重写（最多一次），软伤只进审校提醒。

use crate::models::ReviewIssue;
use crate::styles::StylePreset;

/// 全局 AI 味黑名单（番茄平台专项治理的典型陈词）
pub const AI_FLAVOR_PHRASES: &[&str] = &[
    "头皮发麻",
    "浑身发冷",
    "后背发凉",
    "空气仿佛凝固",
    "空气瞬间凝固",
    "空气似乎凝固",
];

/// 古风 / 纪实风格禁用的现代词
pub const MODERN_WORDS: &[&str] = &[
    "情绪价值",
    "破防",
    "内耗",
    "社死",
    "打工人",
    "内卷",
    "及时止损",
];

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CheckItem {
    pub name: String,
    pub passed: bool,
    /// 不满足时是否算硬伤（硬伤触发自动重写）
    pub hard: bool,
    pub detail: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StyleReport {
    pub style: String,
    pub ok: bool,
    pub hard_fail: bool,
    pub checks: Vec<CheckItem>,
}

impl StyleReport {
    /// 通过数（用于两版稿件择优）
    pub fn score(&self) -> usize {
        self.checks.iter().filter(|c| c.passed).count()
    }

    /// 失败项摘要（注入"重写提示"与审校意见）
    pub fn summary(&self) -> String {
        let failed: Vec<String> = self
            .checks
            .iter()
            .filter(|c| !c.passed)
            .map(|c| format!("{}（{}）", c.name, c.detail))
            .collect();
        if failed.is_empty() {
            "全部通过".to_string()
        } else {
            failed.join("；")
        }
    }

    /// 转为审校意见（硬伤 medium、软伤 low）
    pub fn as_review_issues(&self) -> Vec<ReviewIssue> {
        self.checks
            .iter()
            .filter(|c| !c.passed)
            .map(|c| ReviewIssue {
                severity: if c.hard { "medium" } else { "low" }.to_string(),
                note: format!("文风检查：{} —— {}", c.name, c.detail),
                quote: String::new(),
                status: "open".to_string(),
                is_new: false,
                stale: false,
            })
            .collect()
    }
}

/// 章节文本的轻量统计（正文部分，排除首行标题）
#[derive(Debug, Clone, Copy)]
pub struct TextStats {
    pub chars: usize,
    pub dialogue_ratio: f32,
    pub paragraph_cv: f32,
    pub max_sentence: usize,
}

pub fn text_stats(text: &str) -> TextStats {
    // 首行标题（第N章 …）不计入正文统计
    let body: String = {
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("");
        let is_title = first.trim_start().starts_with('第') && first.contains('章');
        if is_title {
            lines.collect::<Vec<_>>().join("\n")
        } else {
            text.to_string()
        }
    };

    let chars = body.chars().filter(|c| !c.is_whitespace()).count();

    // 对话字符：成对的中文引号/直角引号内的内容
    let dialogue = count_dialog_chars(&body);
    let dialogue_ratio = if chars == 0 {
        0.0
    } else {
        dialogue as f32 / chars as f32
    };

    // 段落：非空行
    let paragraphs: Vec<usize> = body
        .lines()
        .map(|l| l.trim().chars().count())
        .filter(|n| *n > 0)
        .collect();
    let paragraph_cv = coefficient_of_variation(&paragraphs);

    // 句子：按中英文句末标点切分
    let mut max_sentence = 0usize;
    for sentence in body.split(['。', '！', '？', '!', '?', '…', '\n']) {
        let n = sentence
            .chars()
            .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation())
            .count();
        max_sentence = max_sentence.max(n);
    }

    TextStats {
        chars,
        dialogue_ratio,
        paragraph_cv,
        max_sentence,
    }
}

fn count_dialog_chars(text: &str) -> usize {
    let pairs = [('「', '」'), ('『', '』'), ('“', '”'), ('"', '"')];
    let mut total = 0usize;
    for (open, close) in pairs {
        let mut in_dialog = false;
        for ch in text.chars() {
            if ch == open {
                if open == close {
                    // 直引号同一字符成对：开/关交替翻转（否则从第一个引号起全部误算为对话）
                    in_dialog = !in_dialog;
                } else {
                    in_dialog = true;
                }
            } else if ch == close {
                in_dialog = false;
            } else if in_dialog {
                total += 1;
            }
        }
    }
    total
}

fn coefficient_of_variation(values: &[usize]) -> f32 {
    if values.len() < 2 {
        return 0.0;
    }
    let n = values.len() as f32;
    let mean = values.iter().sum::<usize>() as f32 / n;
    if mean <= 0.0 {
        return 0.0;
    }
    let var = values
        .iter()
        .map(|v| {
            let d = *v as f32 - mean;
            d * d
        })
        .sum::<f32>()
        / n;
    var.sqrt() / mean
}

/// 解析 word_target（形如 "2000~2400 字"），返回 (min, max)
pub fn parse_word_target(target: &str) -> Option<(usize, usize)> {
    let nums: Vec<usize> = target
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    match nums.as_slice() {
        [min, max, ..] => Some((*min, *max)),
        [single] => Some((*single, *single)),
        [] => None,
    }
}

/// 章末是否有悬念（启发式）：末段较短、或以问号/省略号/引号收尾、或含钩子词
fn has_ending_hook(text: &str) -> bool {
    let tail: String = {
        let t = text.trim_end();
        let n = t.chars().count();
        t.chars().skip(n.saturating_sub(160)).collect()
    };
    let last_line = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    let last_char = last_line.trim_end().chars().last().unwrap_or('。');
    if matches!(last_char, '？' | '?' | '…' | '」' | '”' | '"') {
        return true;
    }
    const HOOK_WORDS: &[&str] = &[
        "突然", "忽然", "竟然", "难道", "没想到", "可就在", "下一刻", "下一秒", "出乎意料",
        "变故", "冷笑", "皱眉", "沉默", "抬眼",
    ];
    HOOK_WORDS.iter().any(|w| tail.contains(w))
}

/// 对一章正文执行全部检查
pub fn check_chapter(preset: &StylePreset, text: &str) -> StyleReport {
    let stats = text_stats(text);
    let mut checks: Vec<CheckItem> = Vec::new();

    // 1) 段落长短交错（CV ≥ 0.25 通过；< 0.15 判硬伤）
    {
        let cv = stats.paragraph_cv;
        let passed = cv >= 0.25;
        checks.push(CheckItem {
            name: "段落长短交错".into(),
            passed,
            hard: !passed && cv < 0.15,
            detail: format!("段落长度变异系数 {cv:.2}（建议 ≥ 0.25，< 0.15 为 AI 味特征）"),
        });
    }

    // 2) 对话占比（仅当预设要求）
    if let Some(min) = preset.checks.min_dialogue_ratio {
        let ratio = stats.dialogue_ratio;
        let passed = ratio >= min;
        checks.push(CheckItem {
            name: "对话占比".into(),
            passed,
            hard: !passed && ratio < min * 0.8,
            detail: format!("对话占比 {:.0}%（目标 ≥ {:.0}%）", ratio * 100.0, min * 100.0),
        });
    }

    // 3) 单句最长（仅当预设要求）
    if let Some(limit) = preset.checks.max_sentence_len {
        let m = stats.max_sentence;
        let passed = m <= limit;
        checks.push(CheckItem {
            name: "句长上限".into(),
            passed,
            hard: !passed && m > (limit as f32 * 1.3) as usize,
            detail: format!("最长句 {m} 字（上限 {limit}）"),
        });
    }

    // 4) 禁用短语：全局 AI 味 + 风格专属现代词
    {
        let mut hits: Vec<String> = Vec::new();
        for p in AI_FLAVOR_PHRASES {
            if text.contains(p) {
                hits.push((*p).to_string());
            }
        }
        if preset.checks.forbid_modern_words {
            for w in MODERN_WORDS {
                if text.contains(w) {
                    hits.push((*w).to_string());
                }
            }
        }
        let passed = hits.is_empty();
        checks.push(CheckItem {
            name: "禁用短语".into(),
            passed,
            hard: !passed,
            detail: if passed {
                "未命中黑名单".into()
            } else {
                format!("命中：{}", hits.join("、"))
            },
        });
    }

    // 5) 章末悬念（启发式，软失败）
    {
        let passed = has_ending_hook(text);
        checks.push(CheckItem {
            name: "章末悬念".into(),
            passed,
            hard: false,
            detail: if passed {
                "末段具备钩子特征".into()
            } else {
                "末段未见钩子特征（问句/省略号/悬念词），建议检查最后三行".into()
            },
        });
    }

    // 6) 字数窗口（解析预设；偏差过大才软失败）
    if let Some((min, max)) = parse_word_target(preset.word_target) {
        let n = stats.chars;
        let passed = n >= (min as f32 * 0.7) as usize && n <= (max as f32 * 1.3) as usize;
        checks.push(CheckItem {
            name: "单章字数".into(),
            passed,
            hard: !passed && n < min / 2,
            detail: format!("正文 {n} 字（目标 {min}~{max}）"),
        });
    }

    let hard_fail = checks.iter().any(|c| !c.passed && c.hard);
    let ok = checks.iter().all(|c| c.passed);
    StyleReport {
        style: preset.id.to_string(),
        ok,
        hard_fail,
        checks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(id: &str) -> &'static StylePreset {
        crate::styles::find(id).unwrap()
    }

    #[test]
    fn uniform_paragraphs_are_hard_fail() {
        // 完全等长的段落 = AI 味头号特征 → 硬失败
        let text = format!(
            "第1章 测试\n\n{}",
            (0..6)
                .map(|_| "他抬起头看向远方，那里有一片云。".to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );
        let r = check_chapter(preset("xuanyi"), &text);
        let cv_check = r.checks.iter().find(|c| c.name == "段落长短交错").unwrap();
        assert!(!cv_check.passed, "等长段落不应通过");
        assert!(cv_check.hard, "应判为硬伤");
        assert!(r.hard_fail);
    }

    #[test]
    fn varied_paragraphs_pass() {
        let text = "第1章 测试\n\n短句。\n这是一段中等长度的叙述，交代场景与人物状态，长度适中。\n他慢慢把杯子放下，又看了一眼窗外那条被雨淋湿的街，心里忽然升起一个念头，却没有说出口。\n短。\n他又说了一句话，这次稍微长一些，带上了情绪。";
        let r = check_chapter(preset("xuanyi"), text);
        let cv_check = r.checks.iter().find(|c| c.name == "段落长短交错").unwrap();
        assert!(cv_check.passed, "长短交错应通过: {cv_check:?}");
    }

    #[test]
    fn dialogue_ratio_required_for_chatty_styles() {
        // 无对话文本在"发疯毒舌"预设下应判不通过（且为硬伤：0 < 0.42*0.8）
        let text = "第1章 测试\n\n他走进房间，环顾四周，没有人。\n他坐了下来，开始等待。";
        let r = check_chapter(preset("shadiao"), text);
        let d = r.checks.iter().find(|c| c.name == "对话占比").unwrap();
        assert!(!d.passed && d.hard, "{d:?}");

        // 对话充足则通过
        let text2 = "第1章 测试\n\n「你到底来不来？」\n「来。」\n「那就别磨蹭。」\n「行。」\n「走。」";
        let r2 = check_chapter(preset("shadiao"), text2);
        let d2 = r2.checks.iter().find(|c| c.name == "对话占比").unwrap();
        assert!(d2.passed, "{d2:?}");
    }

    #[test]
    fn straight_quotes_toggle_dialog_count() {
        // 直引号同一字符成对：应开/关交替，只统计引号内内容（此前会从第一个引号起全部误算）
        let text = "他说\"别动。\"风停了，灯塔的光扫过海面，所有人沉默着看向远处。";
        assert_eq!(count_dialog_chars(text), "别动。".chars().count());

        // 中文引号仍按左右配对统计
        let text2 = "他问：「走不走？」外面安静极了，没有人回答他的问题。";
        assert_eq!(count_dialog_chars(text2), "走不走？".chars().count());
    }

    #[test]
    fn long_sentence_flagged_for_shadiao_only() {
        let long = "他说".to_string() + &"这句话真的非常非常长而且完全没有停顿一口气说下去让读者喘不过气".repeat(2);
        let text = format!("第1章 测试\n\n{long}。");
        let r = check_chapter(preset("shadiao"), &text);
        assert!(r.checks.iter().any(|c| c.name == "句长上限" && !c.passed));
        // 其他预设没有句长约束，不出现该项
        let r2 = check_chapter(preset("gufeng"), &text);
        assert!(!r2.checks.iter().any(|c| c.name == "句长上限"));
    }

    #[test]
    fn ai_flavor_and_modern_words_are_hard_fail() {
        let text = "第1章 测试\n\n他头皮发麻，空气仿佛凝固。";
        let r = check_chapter(preset("xuanyi"), text);
        let p = r.checks.iter().find(|c| c.name == "禁用短语").unwrap();
        assert!(!p.passed && p.hard, "{p:?}");

        // 现代词只在古风/纪实预设中检查
        let text2 = "第1章 测试\n\n他觉得很破防，决定不再内耗。";
        assert!(
            !check_chapter(preset("gufeng"), text2)
                .checks
                .iter()
                .find(|c| c.name == "禁用短语")
                .unwrap()
                .passed
        );
        assert!(
            check_chapter(preset("xuanyi"), text2)
                .checks
                .iter()
                .find(|c| c.name == "禁用短语")
                .unwrap()
                .passed,
            "普通预设不检查现代词"
        );
    }

    #[test]
    fn word_target_parsing() {
        assert_eq!(parse_word_target("2000~2400 字"), Some((2000, 2400)));
        assert_eq!(parse_word_target("1800~2200 字"), Some((1800, 2200)));
        assert_eq!(parse_word_target("2000 字"), Some((2000, 2000)));
        assert_eq!(parse_word_target("不限"), None);
    }

    #[test]
    fn report_shapes_review_issues() {
        let text = format!("第1章 测\n\n{}", "一样长度的段落内容。\n".repeat(5));
        let r = check_chapter(preset("rexue"), &text);
        let issues = r.as_review_issues();
        assert!(!issues.is_empty());
        assert!(issues.iter().any(|i| i.note.contains("文风检查")));
        assert!(issues.iter().all(|i| i.severity == "medium" || i.severity == "low"));
        assert!(r.summary().contains("段落"));
    }

    #[test]
    fn text_stats_basic_numbers() {
        let s = text_stats("第1章 标题\n\n「你好。」他说。\n\n又是一段。");
        assert!(s.chars > 0);
        assert!(s.dialogue_ratio > 0.0, "应识别引号内对话");
        assert!(s.paragraph_cv > 0.0);
    }
}
