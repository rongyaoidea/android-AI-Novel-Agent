//! 提示词模板（原创、精简版；独立实现，未复制上游仓库文本）。
//! 每个任务在 user 文本中带 `TASK:xxx` 标记，便于离线 Mock 提供方与测试识别。

use crate::models::{Brief, ChapterPlan};
use crate::styles::StylePreset;

/// 风格块（选中预设时追加到任务提示词末尾；未选则返回空串，保持既有行为）
fn style_section(style: Option<&StylePreset>) -> String {
    match style {
        Some(p) => format!("\n\n{}", crate::styles::style_block(p)),
        None => String::new(),
    }
}

pub fn brief_prompt(idea: &str, style: Option<&StylePreset>) -> (String, String) {
    let system = "你是资深中文网文策划。只输出一个 JSON 对象，不要输出任何其他文字，不要使用 Markdown 代码块。".to_string();
    let style_line = match style {
        Some(p) => format!("文风预设：{}（{}）——书名与卖点要贴合该风格。\n", p.name, p.tagline),
        None => String::new(),
    };
    let user = format!(
        "TASK:brief\n想法：{idea}\n{style_line}\n\
请基于上述想法给出一本书的立项方案，输出 JSON，字段：\n\
- title: 书名（12 字以内，有网感）\n\
- genre: 题材（如 玄幻/都市/悬疑/科幻/历史）\n\
- logline: 一句话卖点（40 字以内）\n\
- protagonist: 主角名与一句话设定\n\
- style: 写法基调（节奏 / 视角 / 语言风格；若给了文风预设，需与该预设一致）\n"
    );
    (system, user)
}

/// 章节规划：只针对 [from, to] 区间生成；prev 为已有章节（供连续性参考）。
pub fn outline_prompt(
    brief: &Brief,
    from: i64,
    to: i64,
    prev: &[(i64, String)],
    style: Option<&StylePreset>,
) -> (String, String) {
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
    let style_line = match style {
        Some(p) => format!(
            "文风预设：{}（{}）——章纲的节拍要贴合该风格的爽点/钩子节奏。\n",
            p.name, p.tagline
        ),
        None => String::new(),
    };
    let user = format!(
        "TASK:outline from={from} to={to}\n\
书名：{title}\n题材：{genre}\n卖点：{logline}\n主角：{protagonist}\n基调：{style}\n{style_line}\
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
    let user = format!("{user}{}", style_section(style));
    (system, user)
}

pub fn chapter_prompt(
    brief: &Brief,
    book_outline: &str,
    plan: &ChapterPlan,
    prev_tail: &str,
    ledger: &str,
    style: Option<&StylePreset>,
) -> (String, String) {
    let system = "你是中文网文写手。直接输出章节正文：第一行写章节标题，之后不再出现任何说明性文字、不要客套、不要使用 Markdown 标题符号。".to_string();
    let prev = if prev_tail.trim().is_empty() {
        "（这是本书第一章，无前文。）".to_string()
    } else {
        format!("上一章结尾：\n{prev_tail}")
    };
    let ledger_block = if ledger.trim().is_empty() {
        String::new()
    } else {
        format!("\n【必须保持一致的角色事实与未回收伏笔】\n{}\n", ledger.trim())
    };
    let beats = plan.beats.join("；");
    let words = style.map(|p| p.word_target).unwrap_or("1500~2500 字");
    let user = format!(
        "TASK:chapter index={idx} title={title}\n\
书名：{t}\n题材：{g}\n卖点：{l}\n主角：{p}\n基调：{s}\n全书走向：{outline}\n\n\
本章目标：{goal}\n本章节拍：{beats}\n{ledger}\n{prev}\n\n\
要求：写完整的第 {idx} 章正文，{words}；第一行是「第{idx}章 {title}」；\
延续上一章结尾的情绪与场景；章末留悬念。{style}",
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
        ledger = ledger_block,
        prev = prev,
        words = words,
        style = style_section(style),
    );
    (system, user)
}

pub fn review_prompt(chapter_text: &str, style: Option<&StylePreset>) -> (String, String) {
    let system = "你是严格的网文审稿编辑。只输出一个 JSON 对象，不要输出任何其他文字，不要使用 Markdown 代码块。".to_string();
    let style_note = match style {
        Some(p) => format!(
            "\n另外按文风预设《{}》检查文风违例（如：{}；反例：{}），有则计入 issues（severity 用 low）。\n",
            p.name,
            p.rules.first().copied().unwrap_or(""),
            p.avoid.first().copied().unwrap_or(""),
        ),
        None => String::new(),
    };
    let user = format!(
        "TASK:review\n以下是章节正文：\n---\n{chapter_text}\n---\n{style_note}\n\
请给出一份审校结论，输出 JSON，字段：\n\
- ok: 布尔，是否存在必须修复的问题\n\
- issues: 数组，每项 {{\"severity\": \"high|medium|low|info\", \"note\": \"问题说明\", \"quote\": \"原文片段（可空）\"}}\n\
只关注四类问题：设定冲突、信息超前、角色崩坏、AI 腔表达。没有问题就返回空数组。\n"
    );
    (system, user)
}

/// 选段修文：改写 / 扩写 / 精简 / 去 AI 味（输出仅修改后的段落）
pub fn polish_prompt(
    mode: &str,
    selection: &str,
    chapter_title: &str,
    instruction: &str,
    style: Option<&StylePreset>,
) -> (String, String) {
    let system = "你是中文小说编辑。只输出修改后的文段本身，不要解释、不要加引号、不要使用 Markdown。".to_string();
    let goal = match mode {
        "expand" => "在保持原意与语气的前提下扩写，让内容更丰满（约 1.5 倍长度）",
        "shrink" => "精简，保留关键信息与语气，压缩到约 60% 长度",
        "depolish" => "去掉套话与翻译腔，换成更自然、口语化、有画面感的中文；避免“不禁”“仿佛”“令人”等陈词",
        _ => "改写：保留事实与信息，改善表达与节奏",
    };
    let extra = if instruction.trim().is_empty() {
        String::new()
    } else {
        format!("附加要求：{}\n", instruction.trim())
    };
    let style_line = match style {
        Some(p) => format!("文风预设：{}——修改后必须保持该风格（{}）。\n", p.name, p.tagline),
        None => String::new(),
    };
    let user = format!(
        "TASK:polish mode={mode}\n章节：{chapter_title}\n修改目标：{goal}\n{extra}{style_line}原段落：\n---\n{selection}\n---\n\
只输出修改后的段落（保留原段落中的人名、地名与事实）。\n",
        mode = mode,
        chapter_title = chapter_title,
        goal = goal,
        extra = extra,
        selection = selection,
    );
    (system, user)
}

/// 写后资料整理：抽取角色状态变化、伏笔埋设/回收与章节摘要
pub fn extract_prompt(chapter_idx: i64, text: &str) -> (String, String) {
    let system = "你是小说资料整理助手。只输出一个 JSON 对象，不要输出其他文字、不要使用 Markdown 代码块。".to_string();
    let excerpt: String = text.chars().take(6000).collect();
    let user = format!(
        "TASK:extract chapter_index={chapter_idx}\n以下是第 {chapter_idx} 章正文：\n---\n{excerpt}\n---\n\n\
请抽取本章相关的资料，输出 JSON，字段：\n\
- summary: 本章剧情摘要（80 字以内，供后续章节回顾使用）\n\
- characters: 本章有变化或首次出场的角色，每项 {{\"name\": \"\", \"role\": \"主角/配角/反派等\", \
\"status\": \"active|dead|leave\", \"facts\": [\"关键事实，最多 2 条，每条 30 字以内\"]}}\n\
- foreshadowings_planted: 本章新埋下的伏笔，每项 {{\"title\": \"一句话概括\"}}\n\
- foreshadowings_resolved: 本章回收的已有伏笔（用已埋伏笔的标题，可省略）\n\
没有的字段返回空数组；只统计确实发生的信息，不要臆造。\n",
        chapter_idx = chapter_idx,
        excerpt = excerpt,
    );
    (system, user)
}

/// 光标续写：从前文末尾继续写 chars 字左右
pub fn continue_prompt(
    chapter_title: &str,
    context_before: &str,
    chars: i64,
    instruction: &str,
    style: Option<&StylePreset>,
) -> (String, String) {
    let system = "你是中文网文写手。只输出续写的正文本身，不要重复已有文字、不要解释、不要加标题。".to_string();
    let extra = if instruction.trim().is_empty() {
        String::new()
    } else {
        format!("续写要求：{}\n", instruction.trim())
    };
    let style_line = match style {
        Some(p) => format!("文风预设：{}——续写须延续该风格（{}）。\n", p.name, p.tagline),
        None => String::new(),
    };
    let user = format!(
        "TASK:continue chapter=\"{}\" chars={chars}\n以下是本章已有正文的末尾部分：\n---\n{before}\n---\n{extra}{style_line}\n\
请无缝续写约 {chars} 字：延续语气、场景与人物状态，不要复述上文，直接接下去写。\n",
        chapter_title,
        chars = chars,
        before = context_before,
        extra = extra,
    );
    (system, user)
}

/// 章节重写：regenerate=按章纲重新生成整章；revise=按意见改写现有正文
pub fn rewrite_prompt(
    mode: &str,
    chapter_title: &str,
    goal: &str,
    current: &str,
    instruction: &str,
    style: Option<&StylePreset>,
) -> (String, String) {
    let system = "你是中文网文写手。直接输出完整章节正文：第一行写章节标题，不要任何说明性文字、不要 Markdown 标题符号。".to_string();
    let style_line = match style {
        Some(p) => format!("文风预设：{}——新稿必须符合该风格（{}）。\n", p.name, p.tagline),
        None => String::new(),
    };
    if mode == "regenerate" {
        let user = format!(
            "TASK:rewrite mode=regenerate title={chapter_title}\n本章目标：{goal}\n{extra}{style_line}\n\
请按本章目标重新写一整章（1500~2500 字），第一行为「{chapter_title}」，章末留悬念。\n",
            chapter_title = chapter_title,
            goal = goal,
            extra = if instruction.trim().is_empty() { String::new() } else { format!("额外要求：{}\n", instruction.trim()) },
        );
        return (system, user);
    }
    let excerpt: String = current.chars().take(4000).collect();
    let user = format!(
        "TASK:rewrite mode=revise title={chapter_title}\n改写要求：{instruction}\n{style_line}以下是现有正文：\n---\n{excerpt}\n---\n\
请在保留剧情事实与人物的前提下按改写要求输出完整新稿，第一行为「{chapter_title}」。\n",
        chapter_title = chapter_title,
        instruction = if instruction.trim().is_empty() { "在保留剧情的前提下优化表达、节奏与画面感".to_string() } else { instruction.trim().to_string() },
        excerpt = excerpt,
    );
    (system, user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ChapterPlan;

    fn sample_brief() -> Brief {
        Brief {
            title: "测试书".into(),
            genre: "都市".into(),
            logline: "一句话卖点".into(),
            protagonist: "主角名".into(),
            style: "快节奏".into(),
        }
    }

    fn sample_plan() -> ChapterPlan {
        ChapterPlan {
            idx: 1,
            title: "第一章".into(),
            goal: "开场".into(),
            beats: vec!["节拍A".into()],
        }
    }

    #[test]
    fn without_style_prompts_stay_unchanged() {
        let (_, user) = chapter_prompt(&sample_brief(), "走向", &sample_plan(), "", "", None);
        assert!(user.contains("1500~2500 字"), "未选预设时应保持默认字数窗口");
        assert!(!user.contains("平台节奏基线"), "未选预设时不应出现风格块");
        assert!(!user.contains("【文风预设："), "未选预设时不应出现风格块头");
        let (_, ou) = outline_prompt(&sample_brief(), 1, 2, &[], None);
        assert!(!ou.contains("平台节奏基线"), "未选预设时章纲不应出现风格块");
        let (_, bu) = brief_prompt("灵感", None);
        assert!(!bu.contains("平台节奏基线"), "未选预设时立项不应出现风格块");
    }

    #[test]
    fn style_block_is_injected_into_all_writing_tasks() {
        let p = crate::styles::find("gufeng").unwrap();
        let (_, ch) = chapter_prompt(&sample_brief(), "走向", &sample_plan(), "前文", "台账", Some(p));
        assert!(ch.contains("【文风预设：古风雅致·家宅权谋"), "章节提示词应含风格块");
        assert!(ch.contains(p.word_target), "字数窗口应使用预设值");
        assert!(!ch.contains("1500~2500 字"), "选预设后不应再出现默认字数窗口");
        assert!(ch.contains("——"), "风格块应从新行开始（出现在任务标记之后）");
        assert!(ch.starts_with("TASK:chapter"), "任务标记必须仍在开头");

        let (_, ou) = outline_prompt(&sample_brief(), 1, 2, &[], Some(p));
        assert!(ou.contains("文风预设：古风雅致"));
        assert!(ou.contains("平台节奏基线"), "章纲提示词应含完整风格块");
        assert!(ou.starts_with("TASK:outline"));

        let (_, pu) = polish_prompt("rewrite", "选段内容", "第一章", "", Some(p));
        assert!(pu.contains("文风预设：古风雅致"));
        assert!(pu.contains("原段落："), "修文标记不能被破坏");

        let (_, cu) = continue_prompt("第一章", "前文", 300, "", Some(p));
        assert!(cu.contains("文风预设：古风雅致"));

        let (_, ru) = rewrite_prompt("regenerate", "第一章", "目标", "", "", Some(p));
        assert!(ru.contains("文风预设：古风雅致"));
        let (_, rv) = rewrite_prompt("revise", "第一章", "", "正文", "要求", Some(p));
        assert!(rv.contains("文风预设：古风雅致"));

        let (_, re) = review_prompt("正文内容", Some(p));
        assert!(re.contains("文风违例"), "审校应带风格违例检查");
    }

    #[test]
    fn style_blocks_keep_mock_parsable_markers() {
        // 逐个预设跑一遍，确保追加风格块后 Mock 仍能解析出关键参数
        for p in crate::styles::PRESETS {
            let (_, ch) = chapter_prompt(&sample_brief(), "走向", &sample_plan(), "", "", Some(p));
            assert!(ch.contains("index=1"), "{}: index 标记丢失", p.id);
            assert!(ch.contains("title=第一章"), "{}: title 标记丢失", p.id);
            let (_, cu) = continue_prompt("第一章", "前文", 300, "", Some(p));
            assert!(cu.contains("chars=300"), "{}: chars 标记丢失", p.id);
            let (_, ou) = outline_prompt(&sample_brief(), 1, 3, &[], Some(p));
            assert!(ou.contains("from=1") && ou.contains("to=3"), "{}: 区间标记丢失", p.id);
        }
    }
}
