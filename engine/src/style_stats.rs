//! 文体统计（本地、零 token）：跨章重复句 / 章末类型单一 / 字数异常 / 句式概览。
//!
//! 用途：① 书籍体检「文体统计」区块直接展示（前端零成本刷新）；
//!       ② 卷末体检提示词注入——本地统计出事实，LLM 只做裁定与建议。

use serde::Serialize;
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Serialize)]
pub struct RepeatedSentence {
    pub sentence: String,
    /// 出现的章数
    pub count: usize,
    pub chapters: Vec<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WordOutlier {
    pub idx: i64,
    pub words: i64,
    pub median: i64,
    /// "偏短" | "偏长"
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EndingKind {
    pub kind: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct StyleStats {
    pub chapters: usize,
    pub median_words: i64,
    /// 字数异常（<60% / >180% 中位数）
    pub word_outliers: Vec<WordOutlier>,
    /// 跨章重复句（≥10 字且出现在 ≥2 章），按章数降序，最多 12 条
    pub repeated_sentences: Vec<RepeatedSentence>,
    /// 章末类型分布（疑问/感叹/省略/陈述）
    pub ending_kinds: Vec<EndingKind>,
    pub same_ending_ratio: f64,
    /// 句均长（字符）
    pub avg_sentence_len: f64,
    /// 可直接注入提示词的一行摘要
    pub summary_text: String,
}

/// 句子切分：按句末标点与换行切开，保留长度 ≥ min_len 的句子（去掉首尾空白与引号）
fn sentences(text: &str, min_len: usize) -> Vec<String> {
    text.split(|c: char| matches!(c, '。' | '！' | '？' | '!' | '?' | '\n'))
        .map(|s| {
            s.trim().trim_matches(|c: char| {
                matches!(c, '"' | '\'' | '「' | '」' | '『' | '』' | '“' | '”' | '…' | '.')
            })
            .trim()
        })
        .filter(|s| s.chars().count() >= min_len)
        .map(|s| s.to_string())
        .collect()
}

/// 章末类型：看最后一个非空行的收尾方式
fn ending_kind(text: &str) -> &'static str {
    let last = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if last.ends_with('？') || last.ends_with('?') {
        "疑问"
    } else if last.ends_with('！') || last.ends_with('!') {
        "感叹"
    } else if last.ends_with('…') || last.ends_with("...") || last.contains("……") {
        "省略"
    } else {
        "陈述"
    }
}

/// 计算文体统计。chapters 传 (章号, 正文) ，正文须非空（调用方过滤）。
pub fn compute(chapters: &[(i64, &str)]) -> StyleStats {
    let chapters_n = chapters.len();
    // 字数与中位数
    let mut words: Vec<(i64, i64)> = chapters
        .iter()
        .map(|(idx, c)| (*idx, c.chars().count() as i64))
        .filter(|(_, w)| *w > 0)
        .collect();
    words.sort_by_key(|(_, w)| *w);
    let median = if words.is_empty() {
        0
    } else {
        words[words.len() / 2].1
    };
    let mut word_outliers = Vec::new();
    if median > 0 {
        for (idx, w) in &words {
            if *w < median * 6 / 10 {
                word_outliers.push(WordOutlier {
                    idx: *idx,
                    words: *w,
                    median,
                    kind: "偏短".into(),
                });
            } else if *w > median * 18 / 10 {
                word_outliers.push(WordOutlier {
                    idx: *idx,
                    words: *w,
                    median,
                    kind: "偏长".into(),
                });
            }
        }
    }
    // 跨章重复句（≥10 字，出现在 ≥2 章）
    let mut seen: HashMap<String, BTreeSet<i64>> = HashMap::new();
    for (idx, content) in chapters {
        for s in sentences(content, 10) {
            seen.entry(s).or_default().insert(*idx);
        }
    }
    let mut repeated: Vec<RepeatedSentence> = seen
        .into_iter()
        .filter(|(_, chs)| chs.len() >= 2)
        .map(|(sentence, chs)| RepeatedSentence {
            sentence,
            count: chs.len(),
            chapters: chs.into_iter().collect(),
        })
        .collect();
    repeated.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(b.sentence.chars().count().cmp(&a.sentence.chars().count()))
    });
    repeated.truncate(12);
    // 章末类型分布
    let mut kinds: HashMap<&'static str, usize> = HashMap::new();
    for (_, content) in chapters {
        if content.trim().is_empty() {
            continue;
        }
        *kinds.entry(ending_kind(content)).or_default() += 1;
    }
    let mut ending_kinds: Vec<EndingKind> = kinds
        .into_iter()
        .map(|(kind, count)| EndingKind {
            kind: kind.to_string(),
            count,
        })
        .collect();
    ending_kinds.sort_by(|a, b| b.count.cmp(&a.count));
    let same_ending_ratio = ending_kinds
        .first()
        .map(|e| e.count as f64 / chapters_n.max(1) as f64)
        .unwrap_or(0.0);
    // 句均长
    let mut total = 0usize;
    let mut n = 0usize;
    for (_, content) in chapters {
        for s in sentences(content, 2) {
            total += s.chars().count();
            n += 1;
        }
    }
    let avg_sentence_len = if n == 0 {
        0.0
    } else {
        (total as f64 / n as f64 * 10.0).round() / 10.0
    };
    // 提示词摘要
    let mut summary_text = format!(
        "篇章 {} 章；字数中位数 {} 字；字数异常 {} 处",
        chapters_n,
        median,
        word_outliers.len()
    );
    if let Some(top) = repeated.first() {
        summary_text.push_str(&format!(
            "；跨章重复句 top：「{}」出现在 {} 章",
            top.sentence.chars().take(30).collect::<String>(),
            top.count
        ));
    } else {
        summary_text.push_str("；未发现跨章重复句");
    }
    if let Some(e) = ending_kinds.first() {
        summary_text.push_str(&format!(
            "；章末类型以「{}」为主（{}%）",
            e.kind,
            (same_ending_ratio * 100.0).round()
        ));
    }
    summary_text.push_str(&format!("；句均长 {avg_sentence_len} 字"));
    StyleStats {
        chapters: chapters_n,
        median_words: median,
        word_outliers,
        repeated_sentences: repeated,
        ending_kinds,
        same_ending_ratio,
        avg_sentence_len,
        summary_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_repeated_sentences_and_word_outliers() {
        let long = "风从窗缝里钻进来，桌上的纸页轻轻翻了一下。";
        let c1 = format!("第一段内容比较长一些用于拉高字数。{long}{long}{long}");
        let c2 = format!("另一段内容也不同。{long}{long}");
        let c3 = "短。".to_string();
        let stats = compute(&[(1, c1.as_str()), (2, c2.as_str()), (3, c3.as_str())]);
        assert_eq!(stats.chapters, 3);
        assert!(
            stats
                .repeated_sentences
                .iter()
                .any(|r| r.sentence.contains("风从窗缝里钻进来") && r.chapters == vec![1, 2]),
            "{:?}",
            stats.repeated_sentences
        );
        assert!(
            stats.word_outliers.iter().any(|o| o.idx == 3 && o.kind == "偏短"),
            "{:?}",
            stats.word_outliers
        );
        assert!(stats.summary_text.contains("跨章重复句"), "{}", stats.summary_text);
    }

    #[test]
    fn classifies_chapter_endings() {
        let stats = compute(&[
            (1, "他要把真相找出来？"),
            (2, "门开了！"),
            (3, "答案在很远的远方。"),
            (4, "海浪声退了回去……"),
        ]);
        let kinds: Vec<(&str, usize)> = stats
            .ending_kinds
            .iter()
            .map(|e| (e.kind.as_str(), e.count))
            .collect();
        assert!(kinds.contains(&("疑问", 1)), "{kinds:?}");
        assert!(kinds.contains(&("感叹", 1)), "{kinds:?}");
        assert!(kinds.contains(&("省略", 1)), "{kinds:?}");
        assert!(kinds.contains(&("陈述", 1)), "{kinds:?}");
        assert!((stats.same_ending_ratio - 0.25).abs() < 0.01, "{}", stats.same_ending_ratio);
    }
}
