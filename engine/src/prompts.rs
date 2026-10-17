//! 提示词模板：每个任务 = 默认 system/user 模板 + 变量表；用户可在
//! 「模型设置 → 提示词模板」覆盖任意任务，占位符用 `{{name}}`。
//!
//! 兼容约定：
//! - 默认 user 模板以 `TASK:xxx` 标记开头（离线 Mock 与测试依赖）；
//!   自定义模板若删掉了该标记，渲染时会自动补一行，不影响 Mock 使用；
//! - 模板里的单个 `{` `}` 是普通字符（原样输出），只有 `{{name}}` 才会被替换。

use serde::{Deserialize, Serialize};

use crate::models::{Brief, ChapterPlan};
use crate::styles::StylePreset;

// ---------- 任务与变量定义 ----------

#[derive(Debug, Clone, Serialize)]
pub struct VarSpec {
    pub name: &'static str,
    pub desc: &'static str,
    pub sample: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub desc: &'static str,
    pub vars: &'static [VarSpec],
    pub default_system: &'static str,
    pub default_user: &'static str,
}

static BRIEF: TaskSpec = TaskSpec {
    id: "brief",
    label: "立项方案",
    desc: "根据一句灵感生成书名 / 题材 / 卖点 / 主角 / 基调（JSON）",
    vars: &[
        VarSpec { name: "idea", desc: "小说灵感（用户输入）", sample: "一个能听见鲸歌的少年，在深海城市里寻找失踪的姐姐" },
        VarSpec { name: "style_line", desc: "文风预设提示行（未选预设时为空）", sample: "文风预设：悬疑限知·反转烧脑（镜头扛在主角肩上）——书名与卖点要贴合该风格。\n" },
        VarSpec { name: "clarify_block", desc: "作者已确认的设定块（未做澄清时为空）", sample: "\n【作者已确认的设定（必须遵守）】\nQ: 主角性别与身份？\nA: 女性，港口调音师\n" },
    ],
    default_system: "你是资深中文网文策划。只输出一个 JSON 对象，不要输出任何其他文字，不要使用 Markdown 代码块。",
    default_user: "TASK:brief\n想法：{{idea}}\n{{style_line}}{{clarify_block}}\n\
请基于上述想法给出一本书的立项方案，输出 JSON，字段：\n\
- title: 书名（12 字以内，有网感）\n\
- genre: 题材（如 玄幻/都市/悬疑/科幻/历史）\n\
- logline: 一句话卖点（40 字以内）\n\
- protagonist: 主角名与一句话设定\n\
- worldview: 世界观设定（60 字以内：时代背景、力量体系/核心规则、关键地点或阵营）\n\
- style: 写法基调（节奏 / 视角 / 语言风格；若给了文风预设，需与该预设一致）\n",
};

static CLARIFY: TaskSpec = TaskSpec {
    id: "clarify",
    label: "立项澄清",
    desc: "根据灵感提出关键澄清问题，把核心设定问清楚再动笔（JSON）",
    vars: &[
        VarSpec { name: "idea", desc: "小说灵感（用户输入）", sample: "一个能听见鲸歌的少年，在深海城市里寻找失踪的姐姐" },
        VarSpec { name: "style_line", desc: "文风预设提示行（未选预设时为空）", sample: "文风预设：悬疑限知·反转烧脑——问题可围绕该风格强调的要素展开。\n" },
    ],
    default_system: "你是资深中文网文策划。只输出一个 JSON 对象，不要输出任何其他文字，不要使用 Markdown 代码块。",
    default_user: "TASK:clarify\n想法：{{idea}}\n{{style_line}}\n\
动笔之前，请针对上述想法列出 5~8 个最关键的澄清问题，覆盖这些维度（按想法的缺口取舍，不要面面俱到）：\n\
- 题材与频道（男频/女频、主分类与核心卖点）\n\
- 主角（性别/身份/性格/核心欲望/能力或金手指）\n\
- 世界观关键规则（时代、力量体系或现实规矩、核心地点）\n\
- 主线目标与核心冲突（主角要什么、谁/什么挡着）\n\
- 基调与爽点（燃 / 虐 / 沙雕 / 悬疑…）、节奏偏好\n\
- 感情线（有无 CP、关系走向）\n\
- 篇幅规划（预计章节规模与结局倾向）\n\
针对该想法里**已经明确的信息不要重复问**。\n\
输出 JSON：{\"questions\": [{\"question\": \"问题（一句话）\", \"hint\": \"为什么问 / 期望的回答形式\", \"suggestions\": [\"常见选项1\", \"常见选项2\"]}]}\n\
questions 最多 8 个；suggestions 给 2~4 个常见选项供参考（作者也可自由回答）。\n",
};

static OUTLINE: TaskSpec = TaskSpec {
    id: "outline",
    label: "章节规划",
    desc: "生成全书走向与指定区间的章节计划（JSON）",
    vars: &[
        VarSpec { name: "from", desc: "起始章号", sample: "1" },
        VarSpec { name: "to", desc: "结束章号", sample: "3" },
        VarSpec { name: "count", desc: "章数", sample: "3" },
        VarSpec { name: "title", desc: "书名", sample: "深海鲸歌" },
        VarSpec { name: "genre", desc: "题材", sample: "都市奇幻" },
        VarSpec { name: "logline", desc: "一句话卖点", sample: "少年在深海城市寻找姐姐与鲸歌的源头" },
        VarSpec { name: "protagonist", desc: "主角设定", sample: "林深 —— 在港口长大的调音少年" },
        VarSpec { name: "style", desc: "写法基调（立项生成）", sample: "节奏明快，第三人称限知视角" },
        VarSpec { name: "style_line", desc: "文风预设提示行（未选预设时为空）", sample: "文风预设：悬疑限知·反转烧脑——章纲的节拍要贴合该风格的爽点/钩子节奏。\n" },
        VarSpec { name: "prev_text", desc: "已有章节列表", sample: "无" },
        VarSpec { name: "style_block", desc: "文风完整规则块（未选预设时为空）", sample: "\n\n【文风预设：悬疑限知·反转烧脑】…" },
    ],
    default_system: "你是网文结构编辑。只输出一个 JSON 对象，不要输出任何其他文字，不要使用 Markdown 代码块。",
    default_user: "TASK:outline from={{from}} to={{to}}\n\
书名：{{title}}\n题材：{{genre}}\n卖点：{{logline}}\n主角：{{protagonist}}\n基调：{{style}}\n{{style_line}}\
已有章节：{{prev_text}}\n\n\
请给出全书走向与第 {{from}}~{{to}} 章的章节计划（共 {{count}} 章），输出 JSON，字段：\n\
- book_outline: 全书走向（120 字以内；若已有走向且无需调整，返回空字符串）\n\
- chapters: 数组，每项 {\"idx\": {{from}}, \"title\": \"章节名\", \"goal\": \"本章目标（30 字以内）\", \"beats\": [\"关键情节1\", \"关键情节2\", \"关键情节3\"], \"hook_type\": \"章末钩型（悬念/危机/反转/升级/信息，五选一）\", \"intensity\": 推进强度整数（1-5，1=过渡、3=小高潮、5=阶段/卷末爆点）}\n\
数量必须正好 {{count}} 章，idx 从 {{from}} 连续编号到 {{to}}。\n\
节奏要求：intensity 要有起伏——连续 ≤2 的过渡章不超过 2 章；每 2~3 章至少一个 ≥4；同一 hook_type 不连续出现；章末必须有钩（动作截断 / 疑问绑定 / 信息投放）。{{style_block}}",
};

static CHAPTER: TaskSpec = TaskSpec {
    id: "chapter",
    label: "正文生成",
    desc: "按章纲写完整一章正文（流式）",
    vars: &[
        VarSpec { name: "idx", desc: "章号", sample: "1" },
        VarSpec { name: "title", desc: "章节名", sample: "风起之1" },
        VarSpec { name: "book_title", desc: "书名", sample: "深海鲸歌" },
        VarSpec { name: "genre", desc: "题材", sample: "都市奇幻" },
        VarSpec { name: "logline", desc: "一句话卖点", sample: "少年在深海城市寻找姐姐与鲸歌的源头" },
        VarSpec { name: "protagonist", desc: "主角设定", sample: "林深 —— 在港口长大的调音少年" },
        VarSpec { name: "style", desc: "写法基调", sample: "节奏明快，第三人称限知视角" },
        VarSpec { name: "outline", desc: "全书走向", sample: "从港口小城出发，逐步深入深海文明的秘密。" },
        VarSpec { name: "goal", desc: "本章目标", sample: "开场钩子：姐姐留下的录音笔失效" },
        VarSpec { name: "beats", desc: "本章节拍（分号连接）", sample: "开场钩子；冲突升级；章末悬念" },
        VarSpec { name: "hook_line", desc: "本章节奏要求行（章纲钩型/强度；未标注时为空）", sample: "【本章节奏要求】章末钩型：悬念；推进强度 3/5（1=过渡、3=小高潮、5=卷末爆点）；\n" },
        VarSpec { name: "ledger", desc: "角色事实与未回收伏笔块（可能为空）", sample: "\n【必须保持一致的角色事实与未回收伏笔】\n林深：正在寻找姐姐；录音笔：第1章埋下\n" },
        VarSpec { name: "prev", desc: "上一章结尾块", sample: "上一章结尾：\n他把录音笔塞回口袋，海风把后半句话吹散了。" },
        VarSpec { name: "words", desc: "单章字数窗口", sample: "2000~2400 字" },
        VarSpec { name: "style_block", desc: "文风完整规则块（未选预设时为空）", sample: "\n\n【文风预设：悬疑限知·反转烧脑】…" },
    ],
    default_system: "你是中文网文写手。直接输出章节正文：第一行写章节标题，之后不再出现任何说明性文字、不要客套、不要使用 Markdown 标题符号。",
    default_user: "TASK:chapter index={{idx}} title={{title}}\n\
书名：{{book_title}}\n题材：{{genre}}\n卖点：{{logline}}\n主角：{{protagonist}}\n基调：{{style}}\n全书走向：{{outline}}\n\n\
本章目标：{{goal}}\n本章节拍：{{beats}}\n{{hook_line}}{{ledger}}\n{{prev}}\n\n\
要求：写完整的第 {{idx}} 章正文，{{words}}；第一行是「第{{idx}}章 {{title}}」；\
延续上一章结尾的情绪与场景；章末留悬念。{{style_block}}",
};

static REVIEW: TaskSpec = TaskSpec {
    id: "review",
    label: "章节审校",
    desc: "审校设定冲突 / 信息超前 / 角色崩坏 / AI 腔（JSON）",
    vars: &[
        VarSpec { name: "chapter_text", desc: "章节正文", sample: "第1章 风起之1\n\n他把录音笔塞回口袋……" },
        VarSpec { name: "style_note", desc: "文风违例检查提示（未选预设时为空）", sample: "\n另外按文风预设《悬疑限知·反转烧脑》检查文风违例……\n" },
    ],
    default_system: "你是严格的网文审稿编辑。只输出一个 JSON 对象，不要输出任何其他文字，不要使用 Markdown 代码块。",
    default_user: "TASK:review\n以下是章节正文：\n---\n{{chapter_text}}\n---\n{{style_note}}\n\
请给出一份审校结论，输出 JSON，字段：\n\
- ok: 布尔，是否存在必须修复的问题\n\
- issues: 数组，每项 {\"severity\": \"high|medium|low|info\", \"note\": \"问题说明\", \"quote\": \"原文片段（可空）\"}\n\
只关注四类问题：设定冲突、信息超前、角色崩坏、AI 腔表达。没有问题就返回空数组。\n",
};

static POLISH: TaskSpec = TaskSpec {
    id: "polish",
    label: "选段修文",
    desc: "改写 / 扩写 / 精简 / 去 AI 味（仅输出修改后的段落）",
    vars: &[
        VarSpec { name: "mode", desc: "模式：rewrite/expand/shrink/depolish", sample: "rewrite" },
        VarSpec { name: "chapter_title", desc: "章节名", sample: "风起之1" },
        VarSpec { name: "goal", desc: "按模式的修改目标描述", sample: "改写：保留事实与信息，改善表达与节奏" },
        VarSpec { name: "extra", desc: "用户附加要求行（可空）", sample: "附加要求：再口语一点\n" },
        VarSpec { name: "style_line", desc: "文风预设提示行（未选预设时为空）", sample: "文风预设：悬疑限知·反转烧脑——修改后必须保持该风格（…）。\n" },
        VarSpec { name: "selection", desc: "选中的原段落", sample: "他走了过去，看了看窗户。" },
    ],
    default_system: "你是中文小说编辑。只输出修改后的文段本身，不要解释、不要加引号、不要使用 Markdown。",
    default_user: "TASK:polish mode={{mode}}\n章节：{{chapter_title}}\n修改目标：{{goal}}\n{{extra}}{{style_line}}原段落：\n---\n{{selection}}\n---\n\
只输出修改后的段落（保留原段落中的人名、地名与事实）。\n",
};

static CONTINUE: TaskSpec = TaskSpec {
    id: "continue",
    label: "光标续写",
    desc: "从光标前文无缝续写指定字数",
    vars: &[
        VarSpec { name: "chapter_title", desc: "章节名", sample: "风起之1" },
        VarSpec { name: "chars", desc: "续写字数", sample: "600" },
        VarSpec { name: "before", desc: "光标前文（最多 1500 字）", sample: "雨还在下，他站在巷口。" },
        VarSpec { name: "extra", desc: "用户续写要求行（可空）", sample: "续写要求：多写环境音\n" },
        VarSpec { name: "style_line", desc: "文风预设提示行（未选预设时为空）", sample: "文风预设：悬疑限知·反转烧脑——续写须延续该风格（…）。\n" },
    ],
    default_system: "你是中文网文写手。只输出续写的正文本身，不要重复已有文字、不要解释、不要加标题。",
    default_user: "TASK:continue chapter=\"{{chapter_title}}\" chars={{chars}}\n以下是本章已有正文的末尾部分：\n---\n{{before}}\n---\n{{extra}}{{style_line}}\n\
请无缝续写约 {{chars}} 字：延续语气、场景与人物状态，不要复述上文，直接接下去写。\n",
};

static REWRITE: TaskSpec = TaskSpec {
    id: "rewrite",
    label: "章节重写",
    desc: "regenerate=按章纲重写整章；revise=按意见改写现有正文",
    vars: &[
        VarSpec { name: "mode", desc: "模式：regenerate/revise", sample: "revise" },
        VarSpec { name: "chapter_title", desc: "章节名", sample: "风起之1" },
        VarSpec { name: "goal", desc: "本章目标", sample: "开场钩子：姐姐留下的录音笔失效" },
        VarSpec { name: "instruction", desc: "改写要求行（可空）", sample: "改写要求：更口语一点\n" },
        VarSpec { name: "style_line", desc: "文风预设提示行（未选预设时为空）", sample: "文风预设：悬疑限知·反转烧脑——新稿必须符合该风格（…）。\n" },
        VarSpec { name: "excerpt_block", desc: "现有正文块（regenerate 时为空）", sample: "以下是现有正文：\n---\n……\n---\n" },
        VarSpec { name: "task_line", desc: "任务指令行（按模式预置）", sample: "请在保留剧情事实与人物的前提下按改写要求输出完整新稿，第一行为「风起之1」。" },
    ],
    default_system: "你是中文网文写手。直接输出完整章节正文：第一行写章节标题，不要任何说明性文字、不要 Markdown 标题符号。",
    default_user: "TASK:rewrite mode={{mode}} title={{chapter_title}}\n本章目标：{{goal}}\n{{instruction}}\
{{style_line}}{{excerpt_block}}\n{{task_line}}\n",
};

static EXTRACT: TaskSpec = TaskSpec {
    id: "extract",
    label: "资料整理",
    desc: "写后抽取摘要 / 角色变化 / 伏笔埋设与回收（JSON）",
    vars: &[
        VarSpec { name: "chapter_idx", desc: "章号", sample: "1" },
        VarSpec { name: "excerpt", desc: "章节正文节选（最多 6000 字）", sample: "第1章 风起之1\n\n他把录音笔塞回口袋……" },
    ],
    default_system: "你是小说资料整理助手。只输出一个 JSON 对象，不要输出其他文字、不要使用 Markdown 代码块。",
    default_user: "TASK:extract chapter_index={{chapter_idx}}\n以下是第 {{chapter_idx}} 章正文：\n---\n{{excerpt}}\n---\n\n\
请抽取本章相关的资料，输出 JSON，字段：\n\
- summary: 本章剧情摘要（80 字以内，供后续章节回顾使用）\n\
- characters: 本章有变化或首次出场的角色，每项 {\"name\": \"\", \"role\": \"主角/配角/反派等\", \"status\": \"active|dead|leave\", \"facts\": [\"关键事实，最多 2 条，每条 30 字以内\"]}\n\
- foreshadowings_planted: 本章新埋下的伏笔，每项 {\"title\": \"一句话概括\"}\n\
- foreshadowings_resolved: 本章回收的已有伏笔（用已埋伏笔的标题，可省略）\n\
没有的字段返回空数组；只统计确实发生的信息，不要臆造。\n",
};

static VOLUME_SUMMARY: TaskSpec = TaskSpec {
    id: "volume_summary",
    label: "卷级摘要",
    desc: "每 20 章归并一条卷级摘要，供长书回顾注入（JSON）",
    vars: &[
        VarSpec { name: "from", desc: "本卷起始章号", sample: "1" },
        VarSpec { name: "to", desc: "本卷结束章号", sample: "20" },
        VarSpec { name: "chapter_summaries", desc: "本卷各章摘要清单", sample: "第1章 风起：……\n第2章 暗涌：……" },
    ],
    default_system: "你是小说结构编辑。只输出一个 JSON 对象，不要输出其他文字、不要使用 Markdown 代码块。",
    default_user: "TASK:volume_summary from={{from}} to={{to}}\n以下是第 {{from}}~{{to}} 章逐章摘要：\n---\n{{chapter_summaries}}\n---\n\n\
请把这一卷压缩成一条连贯的卷级摘要（200 字以内），保留：关键事件走向、主要人物变化、尚未回收的悬念。\n\
输出 JSON：{\"summary\": \"……\"}\n",
};

static FORESHADOW_AUDIT: TaskSpec = TaskSpec {
    id: "foreshadow_audit",
    label: "伏笔审核",
    desc: "用章节摘要批量判定未回收伏笔是否已被后文实质回收（JSON）",
    vars: &[
        VarSpec { name: "foreshadow_list", desc: "未回收伏笔清单（含埋设章号）", sample: "- 青铜钥匙的来历（第3章埋下）" },
        VarSpec { name: "chapter_summaries", desc: "各章摘要（供判断是否已回收）", sample: "第3章 钥匙：……\n第7章 门开：……" },
    ],
    default_system: "你是小说连续性审核助手。只输出一个 JSON 对象，不要输出其他文字、不要使用 Markdown 代码块。",
    default_user: "TASK:foreshadow_audit\n未回收伏笔清单：\n{{foreshadow_list}}\n\n各章摘要：\n{{chapter_summaries}}\n\n\
请判断哪些伏笔已经在后文被实质回收（事件落地、谜底揭开或明确交代，而非仅仅被提及）。\n\
输出 JSON：{\"resolved\": [{\"title\": \"伏笔标题\", \"chapter_idx\": 回收所在章号, \"evidence\": \"一句话依据\"}]}\n\
只报告有把握的；没有把握就返回空数组。\n",
};

static GLOBAL_REVIEW: TaskSpec = TaskSpec {
    id: "global_review",
    label: "全局审校",
    desc: "全书视角：跨章一致性 / 节奏 / 支线状态审查（JSON）",
    vars: &[
        VarSpec { name: "title", desc: "书名", sample: "深海鲸歌" },
        VarSpec { name: "genre", desc: "题材", sample: "都市奇幻" },
        VarSpec { name: "logline", desc: "一句话卖点", sample: "少年在深海城市寻找姐姐" },
        VarSpec { name: "worldview", desc: "世界观设定", sample: "近代沿海孤城……" },
        VarSpec { name: "chapter_count", desc: "已有章节数", sample: "36" },
        VarSpec { name: "volume_text", desc: "卷摘要（可为「无」）", sample: "第1卷（至第20章）：……" },
        VarSpec { name: "summaries", desc: "逐章摘要链（尾段截断）", sample: "第1章 风起：……\n第2章 暗涌：……" },
        VarSpec { name: "ledger", desc: "角色与伏笔台账块（可能为空）", sample: "角色事实：\n- 林深（主角·active）：……" },
    ],
    default_system: "你是网文结构编辑兼追读读者。只输出一个 JSON 对象，不要输出其他文字、不要使用 Markdown 代码块。",
    default_user: "TASK:global_review\n书名：{{title}}（{{genre}}）\n卖点：{{logline}}\n世界观：{{worldview}}\n共 {{chapter_count}} 章。\n\n\
【卷摘要】\n{{volume_text}}\n\n【逐章摘要】\n{{summaries}}\n\n【台账】\n{{ledger}}\n\n\
请以「追读读者 + 结构编辑」的双重视角审视全书，找出真正影响追读体验的**跨章问题**（最多 3 条，宁缺毋滥）：\n\
- 一致性：设定/时间线/人物状态是否前后矛盾（引台账核对）\n\
- 节奏：是否连续多章平淡、信息过载、或支线长期失踪\n\
- 结构：主线推进是否停滞、主角目标是否模糊、悬念是否长期悬空\n\
输出 JSON：{\"ok\": true, \"issues\": [{\"severity\": \"high|medium|low\", \"note\": \"问题的一句话描述\", \"affected_chapters\": [涉及章号整数], \"suggestion\": \"修改建议（一句话，指明改哪一章的什么）\"}]}\n\
没有达到标准的跨章问题就返回空数组；不要为了凑数报告小问题。\n",
};

static VOLUME_REVIEW: TaskSpec = TaskSpec {
    id: "volume_review",
    label: "卷末体检",
    desc: "卷边界从留存与爽感角度复查节奏 / 爽点 / 伏笔 / 支线 / 字数（JSON）",
    vars: &[
        VarSpec { name: "volume_idx", desc: "卷号", sample: "1" },
        VarSpec { name: "from", desc: "起始章号", sample: "1" },
        VarSpec { name: "to", desc: "结束章号", sample: "20" },
        VarSpec { name: "volume_summary", desc: "本卷卷级摘要", sample: "本卷行至第 20 章：主角逐步逼近真相……" },
        VarSpec { name: "chapter_lines", desc: "逐章概览（标题/强度/钩型/字数/摘要）", sample: "第1章 风起｜强度2｜钩-危机｜2400字｜主角在码头接到陌生人传话" },
        VarSpec { name: "foreshadow_text", desc: "未回收伏笔（含拖延章数；可能为空）", sample: "录音笔（第1章埋下，拖 15 章）" },
        VarSpec { name: "stylestat_text", desc: "本地文体统计摘要", sample: "篇章 20 章；字数中位数 2400 字；字数异常 1 处；……" },
    ],
    default_system: "你是网文读者代表兼结构编辑。只输出一个 JSON 对象，不要输出其他文字、不要使用 Markdown 代码块。",
    default_user: "TASK:volume_review\n第 {{volume_idx}} 卷（第 {{from}}~{{to}} 章）体检。\n卷摘要：{{volume_summary}}\n\n\
【逐章概览】\n{{chapter_lines}}\n\n【未回收伏笔】\n{{foreshadow_text}}\n\n【文体统计】\n{{stylestat_text}}\n\n\
请从「留存与爽感」角度找出会影响追读的问题（最多 6 条，宁缺毋滥；没有问题就返回空数组），输出 JSON：\n\
- ok: 布尔，是否存在值得修的问题\n\
- findings: 数组，每项 {\"severity\": \"high|medium|low\", \"note\": \"问题（一句话）\", \"chapters\": [涉及章号整数], \"suggestion\": \"可执行建议（一句话，指明改哪一章的什么）\"}\n\
检查维度：节奏起伏（强度曲线是否过平或连续高能）、爽点分布（是否连续多章无推进）、钩子质量（同型连发/空钩）、伏笔逾期、支线失踪、字数异常、与本卷目标的一致性。\n",
};

static READER_REVIEW: TaskSpec = TaskSpec {
    id: "reader_review",
    label: "读者视角评审",
    desc: "按留存要素给指定章节区间打分（代入 / 爽点 / 章末牵引 / 信息密度；JSON）",
    vars: &[
        VarSpec { name: "from", desc: "起始章号", sample: "1" },
        VarSpec { name: "to", desc: "结束章号", sample: "3" },
        VarSpec { name: "book_title", desc: "书名", sample: "深海鲸歌" },
        VarSpec { name: "genre", desc: "题材", sample: "都市奇幻" },
        VarSpec { name: "style_line", desc: "文风预设提示行（未选预设时为空）", sample: "文风预设：悬疑限知·反转烧脑——按该风格的标准评价语言与节奏。\n" },
        VarSpec { name: "golden_note", desc: "黄金三章专项清单（非开篇区间时为空）", sample: "\n【黄金三章专项】按平台开篇留存清单检查：……\n" },
        VarSpec { name: "chapters_text", desc: "逐章输入（目标 / 摘要 / 章末三行）", sample: "【第1章 风起】目标：接到陌生人传话\n摘要：……\n章末三行：\n……" },
    ],
    default_system: "你是最挑剔的网文读者兼编辑。只输出一个 JSON 对象，不要输出其他文字、不要使用 Markdown 代码块。",
    default_user: "TASK:reader_review\n书名：{{book_title}}（{{genre}}）\n{{style_line}}{{golden_note}}\n以下是第 {{from}}~{{to}} 章的关键信息（目标 / 摘要 / 章末三行）：\n---\n{{chapters_text}}\n---\n\n\
请你像一个真实读者一样读完，输出 JSON：\n\
- overall_score: 0-10 的数字（读完还想不想继续追下去）\n\
- overall_comment: 一句话总评（最影响留存的一点）\n\
- chapters: 数组，每项 {\"idx\": 章号, \"score\": 0-10 的数字, \"verdict\": \"一句话评价\", \"issues\": [\"具体问题（可空）\"]}\n\
评分维度：代入感（前 300 字能否抓住人）、爽点/信息密度、章末牵引（钩子是否有效）、节奏（拖沓或赶）、语言（AI 腔/重复）。\n",
};

pub static TASKS: &[&TaskSpec] = &[
    &BRIEF, &CLARIFY, &OUTLINE, &CHAPTER, &REVIEW, &POLISH, &CONTINUE, &REWRITE, &EXTRACT,
    &VOLUME_SUMMARY, &FORESHADOW_AUDIT, &GLOBAL_REVIEW, &VOLUME_REVIEW, &READER_REVIEW,
];

pub fn task(id: &str) -> Option<&'static TaskSpec> {
    TASKS.iter().copied().find(|t| t.id == id)
}

// ---------- 用户覆盖 ----------

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Template {
    pub system: String,
    pub user: String,
}

/// 全部任务覆盖（只保存与默认不同的任务）
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    map: std::collections::BTreeMap<String, Template>,
}

impl Overrides {
    /// 从设置 JSON 解析（宽容：未知任务/畸形字段一律忽略）
    pub fn from_json(raw: Option<&str>) -> Self {
        let mut out = Self::default();
        let Some(raw) = raw else { return out };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
            return out;
        };
        let Some(obj) = v.as_object() else { return out };
        for (k, val) in obj {
            if task(k).is_none() {
                continue;
            }
            if let (Some(system), Some(user)) = (
                val.get("system").and_then(|s| s.as_str()),
                val.get("user").and_then(|s| s.as_str()),
            ) {
                if !system.trim().is_empty() && !user.trim().is_empty() {
                    out.map.insert(
                        k.clone(),
                        Template {
                            system: system.to_string(),
                            user: user.to_string(),
                        },
                    );
                }
            }
        }
        out
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(&self.map).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn get(&self, task_id: &str) -> Option<&Template> {
        self.map.get(task_id)
    }

    pub fn set(&mut self, task_id: &str, t: Template) {
        self.map.insert(task_id.to_string(), t);
    }

    pub fn remove(&mut self, task_id: &str) {
        self.map.remove(task_id);
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// 校验模板：system/user 非空；占位符必须是该任务的已知变量
pub fn validate(spec: &TaskSpec, system: &str, user: &str) -> Result<Template, String> {
    if system.trim().is_empty() {
        return Err("system 不能为空".to_string());
    }
    if user.trim().is_empty() {
        return Err("user 不能为空".to_string());
    }
    for tpl in [system, user] {
        for name in placeholders(tpl) {
            if !spec.vars.iter().any(|v| v.name == name) {
                let known: Vec<&str> = spec.vars.iter().map(|v| v.name).collect();
                return Err(format!(
                    "未知占位符 {{{{{name}}}}}；本任务可用：{}",
                    known.join("、")
                ));
            }
        }
    }
    Ok(Template {
        system: system.to_string(),
        user: user.to_string(),
    })
}

/// 扫描 `{{name}}` 占位符（未闭合的 `{{` 视为非法占位符名，交由校验报错）
fn placeholders(tpl: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = tpl.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let rest = &tpl[i + 2..];
            match rest.find("}}") {
                Some(end) => {
                    out.push(rest[..end].trim().to_string());
                    i += 2 + end + 2;
                }
                None => {
                    out.push(rest.trim().to_string());
                    break;
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

// ---------- 渲染 ----------

/// 渲染：单遍扫描 `{{name}}`（name 允许两侧空白），只在模板位置替换，
/// 变量值里出现的 `{{…}}` 不会被二次替换（避免注入/自展开）。
fn fill(tpl: &str, vars: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let name = after[..end].trim();
                match vars.iter().find(|(k, _)| *k == name) {
                    Some((_, v)) => out.push_str(v),
                    None => {
                        out.push_str("{{");
                        out.push_str(&after[..end]);
                        out.push_str("}}");
                    }
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("{{");
                out.push_str(after);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// 渲染 user：模板（填充前）缺少 `TASK:` 标记时自动补一行（Mock 兼容）。
/// 注意判断的是模板本身而不是渲染结果——变量值里出现 `TASK:` 不该影响补标记。
fn fill_user(spec: &TaskSpec, tpl: &str, vars: &[(&str, String)]) -> String {
    let out = fill(tpl, vars);
    if tpl.contains("TASK:") {
        out
    } else {
        format!("{}\n{}", marker(spec.id, vars), out)
    }
}

/// 合成任务标记行（Mock 解析所需的最小参数集）
fn marker(task_id: &str, vars: &[(&str, String)]) -> String {
    let get = |k: &str| {
        vars.iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    match task_id {
        "chapter" => format!("TASK:chapter index={} title={}", get("idx"), get("title")),
        "outline" => format!("TASK:outline from={} to={}", get("from"), get("to")),
        "continue" => format!("TASK:continue chars={}", get("chars")),
        "polish" => format!("TASK:polish mode={}", get("mode")),
        "rewrite" => format!("TASK:rewrite mode={} title={}", get("mode"), get("chapter_title")),
        "extract" => format!("TASK:extract chapter_index={}", get("chapter_idx")),
        other => format!("TASK:{other}"),
    }
}

fn build(spec: &TaskSpec, ov: &Overrides, vars: &[(&str, String)]) -> (String, String) {
    match ov.get(spec.id) {
        Some(t) => (fill(&t.system, vars), fill_user(spec, &t.user, vars)),
        None => (fill(spec.default_system, vars), fill(spec.default_user, vars)),
    }
}

/// 风格块（选中预设时追加；未选则空串）
fn style_block_var(style: Option<&StylePreset>) -> String {
    match style {
        Some(p) => format!("\n\n{}", crate::styles::style_block(p)),
        None => String::new(),
    }
}

// ---------- 各任务构建 ----------

pub fn brief_prompt(
    idea: &str,
    style: Option<&StylePreset>,
    clarify: &[crate::models::ClarifyQA],
    ov: &Overrides,
) -> (String, String) {
    let style_line = match style {
        Some(p) => format!("文风预设：{}（{}）——书名与卖点要贴合该风格。\n", p.name, p.tagline),
        None => String::new(),
    };
    let clarify_block = build_clarify_block(clarify);
    let vars = vec![
        ("idea", idea.to_string()),
        ("style_line", style_line),
        ("clarify_block", clarify_block),
    ];
    build(&BRIEF, ov, &vars)
}

/// 作者已确认的设定块（立项与后续生成共用的硬约束文本；无问答时为空）
pub fn build_clarify_block(clarify: &[crate::models::ClarifyQA]) -> String {
    let items: Vec<&crate::models::ClarifyQA> = clarify
        .iter()
        .filter(|q| !q.answer.trim().is_empty())
        .collect();
    if items.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n【作者已确认的设定（必须遵守，与其它设定冲突时以此为准）】\n");
    for q in items {
        s.push_str(&format!("Q: {}\nA: {}\n", q.question.trim(), q.answer.trim()));
    }
    s
}

/// 立项澄清提问
pub fn clarify_prompt(idea: &str, style: Option<&StylePreset>, ov: &Overrides) -> (String, String) {
    let style_line = match style {
        Some(p) => format!("文风预设：{}——问题可围绕该风格强调的要素展开。\n", p.name),
        None => String::new(),
    };
    let vars = vec![
        ("idea", idea.to_string()),
        ("style_line", style_line),
    ];
    build(&CLARIFY, ov, &vars)
}

pub fn outline_prompt(
    brief: &Brief,
    from: i64,
    to: i64,
    prev: &[(i64, String)],
    style: Option<&StylePreset>,
    ov: &Overrides,
) -> (String, String) {
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
    let vars = vec![
        ("from", from.to_string()),
        ("to", to.to_string()),
        ("count", count.to_string()),
        ("title", brief.title.clone()),
        ("genre", brief.genre.clone()),
        ("logline", brief.logline.clone()),
        ("protagonist", brief.protagonist.clone()),
        ("style", brief.style.clone()),
        ("style_line", style_line),
        ("prev_text", prev_text),
        ("style_block", style_block_var(style)),
    ];
    build(&OUTLINE, ov, &vars)
}

pub fn chapter_prompt(
    brief: &Brief,
    book_outline: &str,
    plan: &ChapterPlan,
    prev_tail: &str,
    ledger: &str,
    hook_line: &str,
    style: Option<&StylePreset>,
    ov: &Overrides,
) -> (String, String) {
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
    let words = style.map(|p| p.word_target).unwrap_or("1500~2500 字");
    let vars = vec![
        ("idx", plan.idx.to_string()),
        ("title", plan.title.clone()),
        ("book_title", brief.title.clone()),
        ("genre", brief.genre.clone()),
        ("logline", brief.logline.clone()),
        ("protagonist", brief.protagonist.clone()),
        ("style", brief.style.clone()),
        ("outline", book_outline.to_string()),
        ("goal", plan.goal.clone()),
        ("beats", plan.beats.join("；")),
        ("hook_line", hook_line.to_string()),
        ("ledger", ledger_block),
        ("prev", prev),
        ("words", words.to_string()),
        ("style_block", style_block_var(style)),
    ];
    build(&CHAPTER, ov, &vars)
}

pub fn review_prompt(chapter_text: &str, style: Option<&StylePreset>, ov: &Overrides) -> (String, String) {
    let style_note = match style {
        Some(p) => format!(
            "\n另外按文风预设《{}》检查文风违例（如：{}；反例：{}），有则计入 issues（severity 用 low）。\n",
            p.name,
            p.rules.first().copied().unwrap_or(""),
            p.avoid.first().copied().unwrap_or(""),
        ),
        None => String::new(),
    };
    let vars = vec![
        ("chapter_text", chapter_text.to_string()),
        ("style_note", style_note),
    ];
    build(&REVIEW, ov, &vars)
}

pub fn polish_prompt(
    mode: &str,
    selection: &str,
    chapter_title: &str,
    instruction: &str,
    style: Option<&StylePreset>,
    ov: &Overrides,
) -> (String, String) {
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
    let vars = vec![
        ("mode", mode.to_string()),
        ("chapter_title", chapter_title.to_string()),
        ("goal", goal.to_string()),
        ("extra", extra),
        ("style_line", style_line),
        ("selection", selection.to_string()),
    ];
    build(&POLISH, ov, &vars)
}

pub fn continue_prompt(
    chapter_title: &str,
    context_before: &str,
    chars: i64,
    instruction: &str,
    style: Option<&StylePreset>,
    ov: &Overrides,
) -> (String, String) {
    let extra = if instruction.trim().is_empty() {
        String::new()
    } else {
        format!("续写要求：{}\n", instruction.trim())
    };
    let style_line = match style {
        Some(p) => format!("文风预设：{}——续写须延续该风格（{}）。\n", p.name, p.tagline),
        None => String::new(),
    };
    let vars = vec![
        ("chapter_title", chapter_title.to_string()),
        ("chars", chars.to_string()),
        ("before", context_before.to_string()),
        ("extra", extra),
        ("style_line", style_line),
    ];
    build(&CONTINUE, ov, &vars)
}

pub fn rewrite_prompt(
    mode: &str,
    chapter_title: &str,
    goal: &str,
    current: &str,
    instruction: &str,
    style: Option<&StylePreset>,
    ov: &Overrides,
) -> (String, String) {
    let style_line = match style {
        Some(p) => format!("文风预设：{}——新稿必须符合该风格（{}）。\n", p.name, p.tagline),
        None => String::new(),
    };
    let (instruction_line, excerpt_block, task_line) = if mode == "regenerate" {
        (
            if instruction.trim().is_empty() {
                String::new()
            } else {
                format!("额外要求：{}\n", instruction.trim())
            },
            String::new(),
            format!("请按本章目标重新写一整章（1500~2500 字），第一行为「{chapter_title}」，章末留悬念。"),
        )
    } else {
        let excerpt: String = current.chars().take(4000).collect();
        (
            format!(
                "改写要求：{}\n",
                if instruction.trim().is_empty() {
                    "在保留剧情的前提下优化表达、节奏与画面感".to_string()
                } else {
                    instruction.trim().to_string()
                }
            ),
            format!("以下是现有正文：\n---\n{excerpt}\n---\n"),
            format!("请在保留剧情事实与人物的前提下按改写要求输出完整新稿，第一行为「{chapter_title}」。"),
        )
    };
    let vars = vec![
        ("mode", mode.to_string()),
        ("chapter_title", chapter_title.to_string()),
        ("goal", goal.to_string()),
        ("instruction", instruction_line),
        ("style_line", style_line),
        ("excerpt_block", excerpt_block),
        ("task_line", task_line),
    ];
    build(&REWRITE, ov, &vars)
}

pub fn extract_prompt(chapter_idx: i64, text: &str, ov: &Overrides) -> (String, String) {
    let excerpt: String = text.chars().take(6000).collect();
    let vars = vec![
        ("chapter_idx", chapter_idx.to_string()),
        ("excerpt", excerpt),
    ];
    build(&EXTRACT, ov, &vars)
}

/// 卷级摘要：把一卷内各章摘要压成一条（from~to 含端点）
pub fn volume_summary_prompt(
    from: i64,
    to: i64,
    summaries: &[(i64, String, String)],
    ov: &Overrides,
) -> (String, String) {
    let text = summaries
        .iter()
        .map(|(i, t, s)| format!("第{i}章 {t}：{s}"))
        .collect::<Vec<_>>()
        .join("\n");
    let vars = vec![
        ("from", from.to_string()),
        ("to", to.to_string()),
        ("chapter_summaries", text),
    ];
    build(&VOLUME_SUMMARY, ov, &vars)
}

/// 卷末体检：卷摘要 + 逐章概览 + 未回收伏笔 + 文体统计 → findings
#[allow(clippy::too_many_arguments)]
pub fn volume_review_prompt(
    volume_idx: i64,
    from: i64,
    to: i64,
    volume_summary: &str,
    chapter_lines: &str,
    foreshadow_text: &str,
    stylestat_text: &str,
    ov: &Overrides,
) -> (String, String) {
    let vars = vec![
        ("volume_idx", volume_idx.to_string()),
        ("from", from.to_string()),
        ("to", to.to_string()),
        (
            "volume_summary",
            if volume_summary.trim().is_empty() {
                "（暂无）".to_string()
            } else {
                volume_summary.trim().to_string()
            },
        ),
        ("chapter_lines", chapter_lines.to_string()),
        (
            "foreshadow_text",
            if foreshadow_text.trim().is_empty() {
                "无".to_string()
            } else {
                foreshadow_text.to_string()
            },
        ),
        (
            "stylestat_text",
            if stylestat_text.trim().is_empty() {
                "（无）".to_string()
            } else {
                stylestat_text.to_string()
            },
        ),
    ];
    build(&VOLUME_REVIEW, ov, &vars)
}

/// 读者视角评审：指定区间的目标 / 摘要 / 章末三行 → 评分与问题
#[allow(clippy::too_many_arguments)]
pub fn reader_review_prompt(
    brief: &Brief,
    from: i64,
    to: i64,
    golden: bool,
    chapters_text: &str,
    style: Option<&StylePreset>,
    ov: &Overrides,
) -> (String, String) {
    let style_line = match style {
        Some(p) => format!("文风预设：{}——按该风格的标准评价语言与节奏。\n", p.name),
        None => String::new(),
    };
    let golden_note = if golden {
        "\n【黄金三章专项】按平台开篇留存清单检查：第 1 章 300 字内出冲突/悬念且不铺世界观、金手指/核心设定首现、章末有反转钩；第 2 章有打脸/名场面并埋长期伏笔；第 3 章亮出全书核心契约（主角要做什么）；前 1 万字人名 ≤5 个。\n".to_string()
    } else {
        String::new()
    };
    let vars = vec![
        ("from", from.to_string()),
        ("to", to.to_string()),
        ("book_title", brief.title.clone()),
        ("genre", brief.genre.clone()),
        ("style_line", style_line),
        ("golden_note", golden_note),
        ("chapters_text", chapters_text.to_string()),
    ];
    build(&READER_REVIEW, ov, &vars)
}

/// 伏笔自动审核：未回收清单 + 各章摘要 → 判定哪些已实质回收
pub fn foreshadow_audit_prompt(list: &str, summaries: &str, ov: &Overrides) -> (String, String) {
    let vars = vec![
        ("foreshadow_list", list.to_string()),
        ("chapter_summaries", summaries.to_string()),
    ];
    build(&FORESHADOW_AUDIT, ov, &vars)
}

/// 全局审校：全书信息（设定 + 卷摘要 + 逐章摘要 + 台账）→ 跨章问题
#[allow(clippy::too_many_arguments)]
pub fn global_review_prompt(
    title: &str,
    genre: &str,
    logline: &str,
    worldview: &str,
    chapter_count: i64,
    volume_text: &str,
    summaries: &str,
    ledger: &str,
    ov: &Overrides,
) -> (String, String) {
    let vars = vec![
        ("title", title.to_string()),
        ("genre", genre.to_string()),
        ("logline", logline.to_string()),
        ("worldview", if worldview.trim().is_empty() { "（未设定）".to_string() } else { worldview.to_string() }),
        ("chapter_count", chapter_count.to_string()),
        ("volume_text", if volume_text.trim().is_empty() { "无".to_string() } else { volume_text.to_string() }),
        ("summaries", summaries.to_string()),
        ("ledger", if ledger.trim().is_empty() { "（暂无台账）".to_string() } else { ledger.to_string() }),
    ];
    build(&GLOBAL_REVIEW, ov, &vars)
}

// 保留一个 JSON 约束后缀，便于其它模块复用
pub const JSON_SUFFIX: &str = "只输出一个 JSON 对象，不要输出任何其他文字、不要使用 Markdown 代码块。";

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
            worldview: "现代都市，无超自然".into(),
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

    fn none() -> Overrides {
        Overrides::default()
    }

    #[test]
    fn fill_is_single_pass_trims_names_and_marks_from_template() {
        // ① 变量值里的 {{占位符}} 不被二次替换（避免注入/自展开）
        let ov = none();
        let (_, usr) = brief_prompt("他说{{style_line}}真好", None, &[], &ov);
        assert!(
            usr.contains("{{style_line}}"),
            "值里的占位符应原样保留：{usr}"
        );
        // ② 占位符两侧有空格的模板也能渲染（校验通过就应生效）
        let mut o = Overrides::default();
        o.set(
            "brief",
            Template {
                system: "s".into(),
                user: "写 {{ idea }} 吧\nTASK:brief".into(),
            },
        );
        let (_, usr2) = brief_prompt("鲸歌", None, &[], &o);
        assert!(usr2.contains("写 鲸歌 吧"), "{usr2}");
        assert!(!usr2.contains("{{"), "{usr2}");
        // ③ 补 TASK 标记依据「模板本身」：值里出现 TASK: 不该阻止补标记（否则 Mock 会误派任务）
        let mut o2 = Overrides::default();
        o2.set(
            "brief",
            Template {
                system: "s".into(),
                user: "想法：{{idea}}".into(),
            },
        );
        let (_, usr3) = brief_prompt("TASK:chapter index=9", None, &[], &o2);
        assert!(usr3.contains("TASK:brief"), "模板缺标记时应补上：{usr3}");
    }

    #[test]
    fn brief_includes_author_confirmed_clarify() {
        let ov = none();
        let clarify = vec![
            crate::models::ClarifyQA {
                question: "主角性别与身份？".into(),
                answer: "女性，港口调音师".into(),
            },
            crate::models::ClarifyQA {
                question: "（未回答的问题）".into(),
                answer: "".into(),
            },
        ];
        let (_, usr) = brief_prompt("鲸歌", None, &clarify, &ov);
        assert!(usr.contains("作者已确认的设定"), "{usr}");
        assert!(usr.contains("女性，港口调音师"), "{usr}");
        assert!(!usr.contains("未回答的问题"), "未回答的问题不应注入：{usr}");
        // 无澄清时块为空
        let (_, usr2) = brief_prompt("鲸歌", None, &[], &ov);
        assert!(!usr2.contains("作者已确认的设定"), "{usr2}");
    }

    #[test]
    fn chapter_prompt_injects_pacing_line() {
        let ov = none();
        let (_, user) = chapter_prompt(
            &sample_brief(),
            "走向",
            &sample_plan(),
            "",
            "",
            "【本章节奏要求】章末钩型：悬念；推进强度 4/5；\n",
            None,
            &ov,
        );
        assert!(user.contains("本章节奏要求"), "{user}");
        assert!(user.contains("章末钩型：悬念"), "{user}");
        // 无节奏标注时行内容为空（模板占位符替换为空串）
        let (_, user2) = chapter_prompt(&sample_brief(), "走向", &sample_plan(), "", "", "", None, &ov);
        assert!(!user2.contains("本章节奏要求"), "{user2}");
    }

    #[test]
    fn task_catalog_is_consistent() {
        assert_eq!(TASKS.len(), 14, "任务数量应为 14");
        for spec in TASKS {
            assert!(task(spec.id).is_some());
            assert!(!spec.label.is_empty() && !spec.desc.is_empty());
            assert!(!spec.vars.is_empty(), "{} 缺少变量表", spec.id);
            // 默认模板的占位符必须都在变量表里
            for tpl in [spec.default_system, spec.default_user] {
                for name in placeholders(tpl) {
                    assert!(
                        spec.vars.iter().any(|v| v.name == name),
                        "{} 默认模板引用了未声明变量 {{{{{name}}}}}",
                        spec.id
                    );
                }
            }
            // 默认 user 模板必须带任务标记（Mock 依赖）
            assert!(
                spec.default_user.contains(&format!("TASK:{}", spec.id)),
                "{} 默认模板缺少任务标记",
                spec.id
            );
            // 变量名唯一
            let mut names: Vec<&str> = spec.vars.iter().map(|v| v.name).collect();
            let n = names.len();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), n, "{} 变量名重复", spec.id);
        }
    }

    #[test]
    fn validate_rejects_unknown_placeholder() {
        let spec = task("chapter").unwrap();
        let err = validate(spec, "sys", "写一章：{{nope}}").unwrap_err();
        assert!(err.contains("未知占位符"), "{err}");
        let err2 = validate(spec, "  ", "写一章").unwrap_err();
        assert!(err2.contains("system"), "{err2}");
        let ok = validate(spec, "sys", "写第 {{idx}} 章").unwrap();
        assert_eq!(ok.user, "写第 {{idx}} 章");
    }

    #[test]
    fn placeholders_scan() {
        assert_eq!(placeholders("a {{x}} b {{y}}"), vec!["x", "y"]);
        assert_eq!(placeholders("单括号 { 不解析"), Vec::<String>::new());
        assert_eq!(placeholders("{{未}}补"), vec!["未".to_string()]);
    }

    #[test]
    fn overrides_parse_is_forgiving() {
        let ov = Overrides::from_json(Some("not json"));
        assert!(ov.is_empty());
        let ov = Overrides::from_json(Some(r#"{"chapter":{"system":"s","user":"u"},"bogus":{"system":"s","user":"u"},"review":{"system":"","user":"u"}}"#));
        assert!(ov.get("chapter").is_some());
        assert!(ov.get("bogus").is_none(), "未知任务应忽略");
        assert!(ov.get("review").is_none(), "空 system 应忽略");
        // 往返
        let mut ov2 = Overrides::default();
        ov2.set("chapter", Template { system: "s".into(), user: "u".into() });
        let ov3 = Overrides::from_json(Some(&ov2.to_json_string()));
        assert_eq!(ov3.get("chapter").unwrap().user, "u");
    }

    #[test]
    fn custom_override_replaces_default_and_keeps_mock_marker() {
        let mut ov = Overrides::default();
        // 自定义模板：故意不带 TASK 标记
        ov.set("chapter", Template { system: "自定义系统".into(), user: "请写第 {{idx}} 章「{{title}}」，字数 {{words}}。".into() });
        let (sys, user) = chapter_prompt(&sample_brief(), "走向", &sample_plan(), "", "", "", None, &ov);
        assert_eq!(sys, "自定义系统");
        assert!(user.contains("请写第 1 章「第一章」"), "{user}");
        assert!(user.starts_with("TASK:chapter index=1 title=第一章"), "缺标记时应自动补：{user}");
    }

    #[test]
    fn without_style_prompts_stay_unchanged() {
        let ov = none();
        let (_, user) = chapter_prompt(&sample_brief(), "走向", &sample_plan(), "", "", "", None, &ov);
        assert!(user.contains("1500~2500 字"), "未选预设时应保持默认字数窗口");
        assert!(!user.contains("平台节奏基线"), "未选预设时不应出现风格块");
        assert!(!user.contains("【文风预设："), "未选预设时不应出现风格块头");
        let (_, ou) = outline_prompt(&sample_brief(), 1, 2, &[], None, &ov);
        assert!(!ou.contains("平台节奏基线"), "未选预设时章纲不应出现风格块");
        let (_, bu) = brief_prompt("灵感", None, &[], &ov);
        assert!(!bu.contains("平台节奏基线"), "未选预设时立项不应出现风格块");
    }

    #[test]
    fn style_block_is_injected_into_all_writing_tasks() {
        let ov = none();
        let p = crate::styles::find("gufeng").unwrap();
        let (_, ch) = chapter_prompt(&sample_brief(), "走向", &sample_plan(), "前文", "台账", "", Some(p), &ov);
        assert!(ch.contains("【文风预设：古风雅致·家宅权谋"), "章节提示词应含风格块");
        assert!(ch.contains(p.word_target), "字数窗口应使用预设值");
        assert!(!ch.contains("1500~2500 字"), "选预设后不应再出现默认字数窗口");
        assert!(ch.starts_with("TASK:chapter"), "任务标记必须仍在开头");

        let (_, ou) = outline_prompt(&sample_brief(), 1, 2, &[], Some(p), &ov);
        assert!(ou.contains("文风预设：古风雅致"));
        assert!(ou.contains("平台节奏基线"), "章纲提示词应含完整风格块");
        assert!(ou.starts_with("TASK:outline"));

        let (_, pu) = polish_prompt("rewrite", "选段内容", "第一章", "", Some(p), &ov);
        assert!(pu.contains("文风预设：古风雅致"));
        assert!(pu.contains("原段落："), "修文标记不能被破坏");

        let (_, cu) = continue_prompt("第一章", "前文", 300, "", Some(p), &ov);
        assert!(cu.contains("文风预设：古风雅致"));

        let (_, ru) = rewrite_prompt("regenerate", "第一章", "目标", "", "", Some(p), &ov);
        assert!(ru.contains("文风预设：古风雅致"));
        let (_, rv) = rewrite_prompt("revise", "第一章", "", "正文", "要求", Some(p), &ov);
        assert!(rv.contains("文风预设：古风雅致"));

        let (_, re) = review_prompt("正文内容", Some(p), &ov);
        assert!(re.contains("文风违例"), "审校应带风格违例检查");
    }

    #[test]
    fn style_blocks_keep_mock_parsable_markers() {
        // 逐个预设跑一遍，确保追加风格块后 Mock 仍能解析出关键参数
        let ov = none();
        for p in crate::styles::PRESETS {
            let (_, ch) = chapter_prompt(&sample_brief(), "走向", &sample_plan(), "", "", "", Some(p), &ov);
            assert!(ch.contains("index=1"), "{}: index 标记丢失", p.id);
            assert!(ch.contains("title=第一章"), "{}: title 标记丢失", p.id);
            let (_, cu) = continue_prompt("第一章", "前文", 300, "", Some(p), &ov);
            assert!(cu.contains("chars=300"), "{}: chars 标记丢失", p.id);
            let (_, ou) = outline_prompt(&sample_brief(), 1, 3, &[], Some(p), &ov);
            assert!(ou.contains("from=1") && ou.contains("to=3"), "{}: 区间标记丢失", p.id);
        }
    }
}
