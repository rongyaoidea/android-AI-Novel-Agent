//! 提示词模板（原创、精简版；独立实现，未复制上游仓库文本）。
//! 每个任务在 user 文本中带 `TASK:xxx` 标记，便于离线 Mock 提供方与测试识别。

use crate::models::{Brief, ChapterPlan};

pub fn brief_prompt(idea: &str) -> (String, String) {
    let system = "你是资深中文网文策划。只输出一个 JSON 对象，不要输出任何其他文字，不要使用 Markdown 代码块。".to_string();
    let user = format!(
        "TASK:brief\n想法：{idea}\n\n\
请基于上述想法给出一本书的立项方案，输出 JSON，字段：\n\
- title: 书名（12 字以内，有网感）\n\
- genre: 题材（如 玄幻/都市/悬疑/科幻/历史）\n\
- logline: 一句话卖点（40 字以内）\n\
- protagonist: 主角名与一句话设定\n\
- style: 写法基调（节奏 / 视角 / 语言风格）\n"
    );
    (system, user)
}

/// 章节规划：只针对 [from, to] 区间生成；prev 为已有章节（供连续性参考）。
pub fn outline_prompt(brief: &Brief, from: i64, to: i64, prev: &[(i64, String)]) -> (String, String) {
    let system = "你是网文结构编辑。只输出一个 JSON 对象，不要输出任何其他文字，不要使用 Markdown 代码块。".to_string();
    let count = to - from + 1;
    let prev_text = if prev.is_empty() {
        "无".to_string()
    } else {
        prev.iter()
            .map(|(i, t)| format!("第{i}章 {t}"))
            .collect::<Vec<_>>()
            .join("；")
    };
    let user = format!(
        "TASK:outline from={from} to={to}\n\
书名：{title}\n题材：{genre}\n卖点：{logline}\n主角：{protagonist}\n基调：{style}\n\
已有章节：{prev_text}\n\n\
请给出全书走向与第 {from}~{to} 章的章节计划（共 {count} 章），输出 JSON，字段：\n\
- book_outline: 全书走向（120 字以内；若已有走向且无需调整，返回空字符串）\n\
- chapters: 数组，每项 {{\"idx\": {from}, \"title\": \"章节名\", \"goal\": \"本章目标（30 字以内）\", \"beats\": [\"关键情节1\", \"关键情节2\", \"关键情节3\"]}}\n\
数量必须正好 {count} 章，idx 从 {from} 连续编号到 {to}。\n",
        from = from,
        to = to,
        count = count,
        prev_text = prev_text,
        title = brief.title,
        genre = brief.genre,
        logline = brief.logline,
        protagonist = brief.protagonist,
        style = brief.style,
    );
    (system, user)
}

pub fn chapter_prompt(
    brief: &Brief,
    book_outline: &str,
    plan: &ChapterPlan,
    prev_tail: &str,
) -> (String, String) {
    let system = "你是中文网文写手。直接输出章节正文：第一行写章节标题，之后不再出现任何说明性文字、不要客套、不要使用 Markdown 标题符号。".to_string();
    let prev = if prev_tail.trim().is_empty() {
        "（这是本书第一章，无前文。）".to_string()
    } else {
        format!("上一章结尾：\n{prev_tail}")
    };
    let beats = plan.beats.join("；");
    let user = format!(
        "TASK:chapter index={idx} title={title}\n\
书名：{t}\n题材：{g}\n卖点：{l}\n主角：{p}\n基调：{s}\n全书走向：{outline}\n\n\
本章目标：{goal}\n本章节拍：{beats}\n\n{prev}\n\n\
要求：写完整的第 {idx} 章正文，1500~2500 字；第一行是「第{idx}章 {title}」；\
延续上一章结尾的情绪与场景；章末留悬念。\n",
        idx = plan.idx,
        title = plan.title,
        t = brief.title,
        g = brief.genre,
        l = brief.logline,
        p = brief.protagonist,
        s = brief.style,
        outline = book_outline,
        goal = plan.goal,
        beats = beats,
        prev = prev,
    );
    (system, user)
}

pub fn review_prompt(chapter_text: &str) -> (String, String) {
    let system = "你是严格的网文审稿编辑。只输出一个 JSON 对象，不要输出任何其他文字，不要使用 Markdown 代码块。".to_string();
    let user = format!(
        "TASK:review\n以下是章节正文：\n---\n{chapter_text}\n---\n\n\
请给出一份审校结论，输出 JSON，字段：\n\
- ok: 布尔，是否存在必须修复的问题\n\
- issues: 数组，每项 {{\"severity\": \"high|medium|low|info\", \"note\": \"问题说明\", \"quote\": \"原文片段（可空）\"}}\n\
只关注四类问题：设定冲突、信息超前、角色崩坏、AI 腔表达。没有问题就返回空数组。\n"
    );
    (system, user)
}
