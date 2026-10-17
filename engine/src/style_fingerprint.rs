//! 风格指纹（本地、零 token）：把一章/全书正文拆成可统计的「文风指纹」，
//! 并与预设的「指纹带」对照给出偏离告警；同时提供防复述的 n-gram 重叠检测。
//!
//! 依据（docs/文风预设2.0调研.md）：
//! - 可量化维度：段长/句长分布、短句率、标点比与节奏、对话占比、成语密度、套话率、信息熵、功能词；
//! - 目标不是「躲检测」，而是避免预设退化成模板化输出；
//! - 防复述：与风格样本做 12 字连续重叠检测（思想/表达二分法下，逐句复述才是侵权风险）。

use serde::Serialize;
use std::collections::HashMap;

/// 预设指纹带：某指标的期望区间（工程参考带，可调）
#[derive(Debug, Clone, Serialize)]
pub struct FingerprintBand {
    /// 指标名（见 Fingerprint 字段）：avg_para_len / avg_sentence_len / short_sentence_ratio /
    /// comma_period_ratio / quote_ratio / exclaim_per_k / ellipsis_per_k / idiom_per_k
    pub metric: &'static str,
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FingerprintDeviation {
    pub metric: String,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    /// 一句话描述（供日志/界面）
    pub note: String,
}

/// 文风指纹（一章或全书的统计画像）
#[derive(Debug, Clone, Serialize, Default)]
pub struct Fingerprint {
    pub words: i64,
    pub paragraphs: usize,
    /// 平均段长（字）
    pub avg_para_len: f64,
    /// 段落长度变异系数（越大越参差）
    pub para_cv: f64,
    /// 平均句长（字）
    pub avg_sentence_len: f64,
    /// 句长标准差
    pub sentence_sd: f64,
    /// 短句率（≤10 字）
    pub short_sentence_ratio: f64,
    /// 逗号/句号比
    pub comma_period_ratio: f64,
    /// 双引号占比（≈对话量）
    pub quote_ratio: f64,
    /// 感叹号（每千字）
    pub exclaim_per_k: f64,
    /// 省略号（每千字）
    pub ellipsis_per_k: f64,
    /// 成语/四字格（每千字）
    pub idiom_per_k: f64,
    /// 字符信息熵（bits/字）
    pub entropy: f64,
    /// 套话命中（短语 → 次数）
    pub cliches: Vec<(String, usize)>,
    /// 功能词频率（每千字，top 12）
    pub function_words: Vec<(String, f64)>,
}

/// 常见套话清单（AI 味与模板化特征；命中不硬判，只计入指纹）
const CLICHES: &[&str] = &[
    "不由得一愣",
    "眉头紧锁",
    "眉头微皱",
    "深吸一口气",
    "嘴角微微上扬",
    "嘴角勾起",
    "眼神深邃",
    "瞳孔骤缩",
    "头皮发麻",
    "浑身发冷",
    "空气凝固",
    "空气仿佛凝固",
    "心中一凛",
    "心中一惊",
    "淡淡一笑",
    "微微一笑",
    "若有所思",
    "意味深长",
    "不置可否",
    "波澜不惊",
];

/// 中文功能词表（35 项，作者识别黄金特征；这里统计频率用于展示与预设对照）
const FUNCTION_WORDS: &[&str] = &[
    "的", "了", "就", "也", "在", "着", "是", "和", "与", "把", "被", "让", "给", "对", "从",
    "向", "而", "却", "都", "还", "又", "才", "只", "很", "太", "更", "没", "不", "地", "得",
    "吗", "呢", "吧", "啊", "呀",
];

fn is_sentence_end(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '!' | '?' | '\n')
}

/// 计算指纹。空文本返回默认值。
pub fn compute(text: &str) -> Fingerprint {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    let mut fp = Fingerprint {
        words: total as i64,
        ..Default::default()
    };
    if total == 0 {
        return fp;
    }

    // 段落（按换行；去空行）
    let paras: Vec<usize> = text
        .lines()
        .map(|l| l.trim().chars().count())
        .filter(|n| *n > 0)
        .collect();
    fp.paragraphs = paras.len();
    if !paras.is_empty() {
        let mean = paras.iter().sum::<usize>() as f64 / paras.len() as f64;
        fp.avg_para_len = round1(mean);
        if paras.len() > 1 && mean > 0.0 {
            let var = paras
                .iter()
                .map(|n| (*n as f64 - mean).powi(2))
                .sum::<f64>()
                / paras.len() as f64;
            fp.para_cv = round2(var.sqrt() / mean);
        }
    }

    // 句子（按句末标点与换行切分；长度 ≥2 才计入）
    let mut sentences: Vec<usize> = Vec::new();
    let mut cur = 0usize;
    for c in &chars {
        if is_sentence_end(*c) {
            if cur >= 2 {
                sentences.push(cur);
            }
            cur = 0;
        } else {
            cur += 1;
        }
    }
    if cur >= 2 {
        sentences.push(cur);
    }
    if !sentences.is_empty() {
        let mean = sentences.iter().sum::<usize>() as f64 / sentences.len() as f64;
        fp.avg_sentence_len = round1(mean);
        let var = sentences
            .iter()
            .map(|n| (*n as f64 - mean).powi(2))
            .sum::<f64>()
            / sentences.len() as f64;
        fp.sentence_sd = round1(var.sqrt());
        let short = sentences.iter().filter(|n| **n <= 10).count();
        fp.short_sentence_ratio = round2(short as f64 / sentences.len() as f64);
    }

    // 标点
    let commas = text.matches(['，', ',']).count() as f64;
    let periods = text.matches(['。']).count() as f64;
    if periods > 0.0 {
        fp.comma_period_ratio = round2(commas / periods);
    } else if commas > 0.0 {
        fp.comma_period_ratio = commas; // 全逗号无句号：极端值原样暴露
    }
    let quoted: usize = {
        // 中文引号与英文引号内的字符数（近似对话量）
        let mut inside = 0usize;
        let mut open: Option<char> = None;
        for c in &chars {
            match (*c, open) {
                ('“', None) => open = Some('“'),
                ('“', Some('“')) => open = None,
                ('「', None) => open = Some('「'),
                ('」', Some('「')) => open = None,
                ('"', None) => open = Some('"'),
                ('"', Some('"')) => open = None,
                _ => {
                    if open.is_some() {
                        inside += 1;
                    }
                }
            }
        }
        inside
    };
    fp.quote_ratio = round2(quoted as f64 / total as f64);
    let per_k = |n: usize| round2(n as f64 * 1000.0 / total as f64);
    fp.exclaim_per_k = per_k(text.matches(['！', '!']).count());
    fp.ellipsis_per_k = per_k(text.matches("……").count() + text.matches("…").count());

    // 成语/四字格：连续 4 个汉字且非全部常用功能字（近似）
    let mut idiom = 0usize;
    let mut run = 0usize;
    for c in &chars {
        if is_han(*c) {
            run += 1;
        } else {
            if run == 4 {
                idiom += 1;
            }
            run = 0;
        }
    }
    if run == 4 {
        idiom += 1;
    }
    fp.idiom_per_k = per_k(idiom);

    // 信息熵（字符分布）
    let mut freq: HashMap<char, usize> = HashMap::new();
    for c in &chars {
        if !c.is_whitespace() {
            *freq.entry(*c).or_insert(0) += 1;
        }
    }
    let effective: usize = freq.values().sum();
    if effective > 0 {
        let mut h = 0.0f64;
        for v in freq.values() {
            let p = *v as f64 / effective as f64;
            h -= p * p.log2();
        }
        fp.entropy = round2(h);
    }

    // 套话
    let mut cliches = Vec::new();
    for phrase in CLICHES {
        let n = text.matches(phrase).count();
        if n > 0 {
            cliches.push((phrase.to_string(), n));
        }
    }
    cliches.sort_by(|a, b| b.1.cmp(&a.1));
    fp.cliches = cliches;

    // 功能词（每千字；取频率 top 12）
    let mut fw: Vec<(String, f64)> = FUNCTION_WORDS
        .iter()
        .map(|w| (w.to_string(), per_k(text.matches(*w).count())))
        .filter(|(_, v)| *v > 0.0)
        .collect();
    fw.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fw.truncate(12);
    fp.function_words = fw;

    fp
}

/// 指标取值（用于指纹带对照）
pub fn value_of(fp: &Fingerprint, metric: &str) -> Option<f64> {
    match metric {
        "avg_para_len" => Some(fp.avg_para_len),
        "para_cv" => Some(fp.para_cv),
        "avg_sentence_len" => Some(fp.avg_sentence_len),
        "sentence_sd" => Some(fp.sentence_sd),
        "short_sentence_ratio" => Some(fp.short_sentence_ratio),
        "comma_period_ratio" => Some(fp.comma_period_ratio),
        "quote_ratio" => Some(fp.quote_ratio),
        "exclaim_per_k" => Some(fp.exclaim_per_k),
        "ellipsis_per_k" => Some(fp.ellipsis_per_k),
        "idiom_per_k" => Some(fp.idiom_per_k),
        "entropy" => Some(fp.entropy),
        _ => None,
    }
}

/// 与指纹带对照：返回偏离项（超出 [min,max] 即计入；偏离幅度过大时注明「明显」）
pub fn compare(fp: &Fingerprint, bands: &[FingerprintBand]) -> Vec<FingerprintDeviation> {
    let mut out = Vec::new();
    for b in bands {
        let Some(v) = value_of(fp, b.metric) else {
            continue;
        };
        if v >= b.min && v <= b.max {
            continue;
        }
        let mid = (b.min + b.max) / 2.0;
        let off = if v < b.min { b.min - v } else { v - b.max };
        let severe = mid > 0.0 && off / mid > 0.4;
        out.push(FingerprintDeviation {
            metric: b.metric.to_string(),
            value: v,
            min: b.min,
            max: b.max,
            note: format!(
                "{}{}（当前 {}，目标 {}-{}）",
                metric_label(b.metric),
                if severe { "明显偏离" } else { "轻微偏离" },
                v,
                b.min,
                b.max
            ),
        });
    }
    out
}

/// 已知指标名 → 'static 名（供自定义风格的指纹带解析）
pub fn static_metric_name(m: &str) -> Option<&'static str> {
    match m {
        "avg_para_len" => Some("avg_para_len"),
        "para_cv" => Some("para_cv"),
        "avg_sentence_len" => Some("avg_sentence_len"),
        "sentence_sd" => Some("sentence_sd"),
        "short_sentence_ratio" => Some("short_sentence_ratio"),
        "comma_period_ratio" => Some("comma_period_ratio"),
        "quote_ratio" => Some("quote_ratio"),
        "exclaim_per_k" => Some("exclaim_per_k"),
        "ellipsis_per_k" => Some("ellipsis_per_k"),
        "idiom_per_k" => Some("idiom_per_k"),
        "entropy" => Some("entropy"),
        _ => None,
    }
}

pub fn metric_label(metric: &str) -> &'static str {
    match metric {
        "avg_para_len" => "段均长",
        "para_cv" => "段长参差度",
        "avg_sentence_len" => "句均长",
        "sentence_sd" => "句长波动",
        "short_sentence_ratio" => "短句率",
        "comma_period_ratio" => "逗号/句号比",
        "quote_ratio" => "对话占比",
        "exclaim_per_k" => "感叹号密度",
        "ellipsis_per_k" => "省略号密度",
        "idiom_per_k" => "四字格密度",
        "entropy" => "信息熵",
        _ => "指标",
    }
}

/// 防复述：与样本集合做 n 字连续重叠检测，返回第一处命中的重叠片段（截断展示）
pub fn first_overlap(text: &str, samples: &[String], n: usize) -> Option<String> {
    let text_chars: Vec<char> = text.chars().collect();
    if text_chars.len() < n {
        return None;
    }
    let windows: std::collections::HashSet<String> = text_chars
        .windows(n)
        .map(|w| w.iter().collect::<String>())
        .collect();
    for sample in samples {
        let sc: Vec<char> = sample.chars().collect();
        if sc.len() < n {
            continue;
        }
        for w in sc.windows(n) {
            let probe: String = w.iter().collect();
            if windows.contains(&probe) {
                return Some(probe);
            }
        }
    }
    None
}

fn is_han(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}')
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_counts_core_metrics() {
        let text = "他推开门。屋里很暗，只有一盏灯。\n\n“你来了。”老人抬起头，“坐吧。”\n\n他坐下，没有立刻开口。";
        let fp = compute(text);
        assert!(fp.words > 0);
        assert!(fp.paragraphs >= 3, "段落应 ≥3：{fp:?}");
        assert!(fp.avg_sentence_len > 0.0);
        assert!(fp.quote_ratio > 0.0, "应识别引号内对话：{fp:?}");
        assert!(fp.comma_period_ratio > 0.0);
        assert!(fp.entropy > 0.0);
    }

    #[test]
    fn short_sentence_ratio_and_cliches() {
        let text = "他愣住。\n\n他不由得一愣，眉头紧锁。\n\n走吧。";
        let fp = compute(text);
        assert!(fp.short_sentence_ratio > 0.5, "{fp:?}");
        assert!(fp.cliches.iter().any(|(p, _)| p == "不由得一愣"), "{fp:?}");
        assert!(fp.cliches.iter().any(|(p, _)| p == "眉头紧锁"), "{fp:?}");
    }

    #[test]
    fn bands_flag_deviations() {
        let fp = Fingerprint {
            avg_para_len: 60.0,
            short_sentence_ratio: 0.5,
            ..Default::default()
        };
        let bands = vec![
            FingerprintBand { metric: "avg_para_len", min: 15.0, max: 35.0 },
            FingerprintBand { metric: "short_sentence_ratio", min: 0.3, max: 0.6 },
        ];
        let devs = compare(&fp, &bands);
        assert_eq!(devs.len(), 1, "{devs:?}");
        assert_eq!(devs[0].metric, "avg_para_len");
        assert!(devs[0].note.contains("段均长"), "{}", devs[0].note);
    }

    #[test]
    fn overlap_detection_finds_copied_run() {
        let sample = "风从窗缝里钻进来，桌上的纸页轻轻翻了一下，像在提醒他时间不多。".to_string();
        let copied = "他停住脚步。风从窗缝里钻进来，桌上的纸页轻轻翻了一下，像在提醒他时间不多。然后他转身。";
        let clean = "他停住脚步。外面的雨声忽然大了，敲在铁皮棚顶上，一阵紧过一阵。";
        assert!(first_overlap(copied, &[sample.clone()], 12).is_some());
        assert!(first_overlap(clean, &[sample], 12).is_none());
    }
}
