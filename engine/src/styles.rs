//! 写作风格预设：基于番茄小说生态调研蒸馏（见 docs/风格调研-番茄.md）。
//!
//! 设计原则：
//! - 预设以「风格类型」命名，不以作者命名（规避版权与模仿风险）；
//! - 每条规则必须**可执行、可自检**（量化字数/占比/频率），不写「文采好」这类空话；
//! - 选中预设时，平台通用节奏基线与风格规则一起注入生成提示词；未选预设保持原有行为。
//!
//! 注意：注入文本中禁止出现 `=`、`TASK:`、`想法：` 等 Mock 提供方用于解析的标记。

use serde::Serialize;

/// 可本地检查的量化约束（配合 style_check.rs；`None` = 不做该项检查）
#[derive(Debug, Clone, Serialize)]
pub struct StyleChecks {
    /// 最低对话占比（0.42 = 42%）
    pub min_dialogue_ratio: Option<f32>,
    /// 单句最长字数
    pub max_sentence_len: Option<usize>,
    /// 是否禁用现代词（古风 / 纪实）
    pub forbid_modern_words: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StylePreset {
    pub id: &'static str,
    pub name: &'static str,
    /// 频道：男频 / 女频 / 通用
    pub group: &'static str,
    /// 一句话定位（UI 显示）
    pub tagline: &'static str,
    /// 适用题材（UI 显示）
    pub genres: &'static str,
    /// 关键参数（UI 显示：[标签, 值]）
    pub traits: &'static [(&'static str, &'static str)],
    /// 单章字数窗口
    pub word_target: &'static str,
    /// 本地可检查的量化约束
    pub checks: StyleChecks,
    /// 写作规则（注入提示词）
    pub rules: &'static [&'static str],
    /// 反例清单（注入提示词；出现即视为不合格）
    pub avoid: &'static [&'static str],
    /// 典型开场参考（注入提示词）
    pub opening: &'static str,
    /// 典型章末参考（注入提示词）
    pub ending: &'static str,
}

/// 番茄平台通用节奏基线（选中任意预设时一并注入）
pub const PLATFORM_BASELINE: &[&str] = &[
    "单章 {words}，前 300 字内出冲突或悬念，不铺垫世界观",
    "每章至少 1 个爽点或信息爆点；平路（无进展）连续不超过 3 章",
    "章末必须留未解决问题：最后三行采用「动作截断 / 疑问绑定主角命运 / 信息投放」之一，禁止与主线无关的空谜语",
    "段落长短交错、事件密度有快有慢；禁止所有段落等长、节奏匀速（AI 味头号特征）",
    "回忆杀不超过 300 字且必须与当前剧情强相关；不写童年、不做大跨度时间跳跃",
    "同一情绪形容词禁止跨场景重复使用（头皮发麻、浑身发冷、空气凝固一类一律不用）",
    "全书固定视角与人称，同一段内不跳视角",
];

pub static PRESETS: &[StylePreset] = &[
    StylePreset {
        id: "xuanyi",
        name: "悬疑限知·反转烧脑",
        group: "通用",
        tagline: "镜头扛在主角肩上，读者与主角同步失衡",
        genres: "无限流 / 规则怪谈 / 悬疑推理 / 末世求生 / 盗墓",
        traits: &[("视角", "第三人称严格限知"), ("节奏", "每章 1 个谜题环节"), ("爽点", "反转揭示，非战力碾压")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        rules: &[
            "第三人称严格限知：主角看不到、不知道的信息一律不写（包括上帝视角的环境描写）",
            "每章推进 1 个谜题环节，章末必须留下至少 1 个未解事实",
            "每段推理之后紧跟一次自我反证（先否定再重建），禁止一次推理直达答案",
            "规则类信息以独立文本块呈现（告示/信件/日记/守则），一次不超过 3 条",
            "每次脱险必须付出明确代价（记忆/队友/资源/身体），禁止零成本破局",
            "禁止金手指面板与无敌开局；主角能力必须有明确代价",
        ],
        avoid: &[
            "与主线无关的空谜语钩子（抛神秘信号后不交代）",
            "连续两章没有信息增量",
            "用「其实一切都是梦/幻觉」结算剧情",
        ],
        opening: "开场即进入封闭场景或事件，第一条规则立刻生效，不解释世界观",
        ending: "一个早已登场、被当作路人的角色被揭出「他比所有人先来过这里」",
    },
    StylePreset {
        id: "rexue",
        name: "热血高武·少年群像",
        group: "男频",
        tagline: "牺牲与守护是主旋律，台词要能被截图二创",
        genres: "都市高武 / 神话复苏 / 末世 / 群像战争",
        traits: &[("句式", "战斗 6~15 字短句连打"), ("抒情", "≤ 单章 25%"), ("群像", "每卷 1 个配角高光")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        rules: &[
            "战斗段落用 6~15 字短句连打；抒情段落用 20~40 字排比，但抒情合计不超过本章 25%",
            "每卷至少给 1 个非主角角色一整场独立高光（配角可以先于主角变强）",
            "允许以一句台词独立成段收尾，该台词必须「可截图、有传播度」",
            "新名词（境界/势力/神话体）首次出现必须配一句人话解释",
            "打脸必须发生在有代价的情境中；主角可以有原则冲突，但禁止绝对正义的圣母行为",
            "情绪配比参考：热血 4、悲壮 3、轻喜剧 2、日常 1",
        ],
        avoid: &["无脑堆砌打脸桥段", "连续两章都在抒情", "配角工具人化（只为主角陪衬）"],
        opening: "主角出场带缺陷或误解，首章末揭示他与某个大设定绑定",
        ending: "一个刚建立感情的配角做出不可逆的选择，最后一行是他说的最后一句话",
    },
    StylePreset {
        id: "xitong",
        name: "系统神豪·都市短爽",
        group: "男频",
        tagline: "金手指要能算账，每章兑现一个可视化爽点",
        genres: "都市 / 神豪 / 签到系统 / 逆袭打脸",
        traits: &[("爽点", "每章 1 次立刻兑现"), ("对话", "≥ 35%"), ("规则", "全书一致可算账")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: Some(0.33), max_sentence_len: None, forbid_modern_words: false },
        rules: &[
            "金手指的奖励条件、冷却、叠加方式第一次出现就写清，全书保持一致，读者能算账",
            "每章至少 1 个立刻兑现的爽点（到账/打脸/身份曝光），兑现过程要有具体画面",
            "短句为主，信息段落与冲突段落交替；段落长度明显交错",
            "对话占比 35% 以上，对话承担打脸与信息宣告的功能",
            "数字必须精确且前后一致（金额、时间、人数）；角色说了几个字，下文不许写错",
            "配角反应要给具体细节（表情、动作、身份），禁止「全场震惊」类空泛描写",
        ],
        avoid: &["系统规则前后矛盾（番茄差评第一来源）", "一章连打三次脸导致麻木", "主角降智配合剧情"],
        opening: "主角开场处于被轻视或被剥夺的处境，第一章内获得金手指并当场验证一次",
        ending: "亮出主角刚被低估的底牌的一半，或揭示金手指的隐藏条款",
    },
    StylePreset {
        id: "xuanhuan",
        name: "玄幻打脸·废材升级",
        group: "男频",
        tagline: "废材→觉醒→打脸→再被轻视→再打脸的强循环",
        genres: "东方玄幻 / 升级流 / 废材逆袭",
        traits: &[("打脸", "每 2~3 章一次"), ("憋屈", "不超过 1 章"), ("改编", "每章 1 个 30 秒冲突画面")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        rules: &[
            "每 2~3 章一次小打脸；打脸双方的地位差距要写清，围观群众要有名字有身份",
            "境界/器/阶术语全书统一，用词从简，避免生僻自造词堆砌",
            "对话推进信息、短句为主；冲突要给具体招式与代价，不写抽象的气势对抗",
            "每章至少一个可直接拍成 30 秒短剧的冲突画面（可视化优先）",
            "憋屈/铺垫不超过 1 章，铺垫即兑现",
        ],
        avoid: &["退婚→打脸→升级三件套连续重复", "反派智商骤降", "连续灌水日常"],
        opening: "主角天赋被判死或家族被欺压的具体一幕，第一章末觉醒或预示",
        ending: "把主角刚被低估的底牌亮一半，留对手反应",
    },
    StylePreset {
        id: "goudao",
        name: "苟道修仙·日常种田",
        group: "男频",
        tagline: "活得久就是最大的金手指，用时间复利代替战斗爽感",
        genres: "修仙 / 种田 / 经营 / 年代日常",
        traits: &[("战斗", "≤ 单章 15%"), ("情绪", "温情 4 幽默 4 战斗 2"), ("结构", "时间流逝感")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        rules: &[
            "每章允许 0 场战斗，但必须有 1 个「时间流逝感」细节（同辈老去/季节更替/物价变化）",
            "战斗一笔带过（不超过全章 15%），结果碾压胜过过程描写",
            "主角行为准则写死：不主动招惹、不发刀子；语气轻松诙谐，允许现代口语与古风术语混用",
            "章末用「不经意反差式」钩子：日常中露出一点不该有的东西",
            "每卷至少回收一次身边人的情感线，避免纯流水账",
            "长线必须始终有一条贯穿目标，禁止主线涣散",
        ],
        avoid: &["后宫", "无理由卷入大冲突", "后期丢掉主线（同类作品公认败笔）"],
        opening: "主角以不起眼的日常身份入场（大龄起步/打工/种田），第一章交代他的「不争」",
        ending: "强敌围上来，主角慢慢掏出某个与身份完全不符的东西",
    },
    StylePreset {
        id: "jishi",
        name: "第一人称·纪实江湖",
        group: "男频",
        tagline: "用真实地名、器物与行规制造「这是真的」的错觉",
        genres: "盗墓 / 鉴宝 / 行业文 / 年代 / 民国",
        traits: &[("人称", "第一人称回忆录体"), ("真实感", "每章 1 个可查证锚点"), ("爽点", "知识碾压")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: true },
        rules: &[
            "第一人称「我」回忆录体，语气平实，允许自嘲与疲惫感",
            "每章至少 1 个可查证的真实地名/器物/术语做真实感锚点",
            "行业黑话首次出现自然带出含义，不注释、不掉书袋",
            "年代词纪律：严禁出现故事年代之后的网络新词",
            "单线叙事，不交叉多线；以「一件事」为单位推进",
            "爽点来自知识碾压（鉴定/行规反杀），不靠金手指；江湖义气可以与疲惫感并存",
        ],
        avoid: &["现代网络梗", "主角开挂式解谜", "大段历史科普"],
        opening: "「我是XX人」式起手，交代入行缘由与年代背景，一件事紧跟一件事",
        ending: "危险逼近或身份暴露：章末有人识破主角",
    },
    StylePreset {
        id: "shadiao",
        name: "发疯毒舌·金句怼人",
        group: "通用",
        tagline: "替读者泄愤，每章一句能截图的话",
        genres: "娱乐圈 / 穿书 / 恋综 / 都市发疯",
        traits: &[("对话", "≥ 45%"), ("金句", "每章 1 句独立成段"), ("场景", "公共场合")],
        word_target: "1800~2200 字",
        checks: StyleChecks { min_dialogue_ratio: Some(0.42), max_sentence_len: Some(45), forbid_modern_words: false },
        rules: &[
            "对话占比 45% 以上；叙述句尽量短，禁止超过 40 字的长句",
            "每章至少 1 句「可截图金句」，独立成段、放在段落末尾",
            "发疯或毒舌必须绑定一个真实的社会现象或行业乱象，禁止无差别攻击",
            "每个爽点场景放在公共场合（节目现场/发布会/直播间/家族宴），保证可视化",
            "开场 3 句内出冲突；主角第一章必须反击一次",
            "用吐槽代替抒情；禁用「空气仿佛凝固」这类陈词",
        ],
        avoid: &["无逻辑发疯（每次发疯必须解构一个真实现象）", "配角全员降智只为衬托主角", "毒舌变成刻薄霸凌"],
        opening: "主角穿书或翻车后发现自己已被全网黑、家族弃，第一章立刻用一句毒舌回击",
        ending: "一句看似玩笑的话让全场安静",
    },
    StylePreset {
        id: "danvzhu",
        name: "无 CP 大女主·独狼升级",
        group: "女频",
        tagline: "去恋爱线，抢机缘、背后搞事、单飞成长",
        genres: "游戏降临 / 末世 / 无限流 / 穿书 / 大女主",
        traits: &[("感情线", "零恋爱线"), ("爽点", "每 10 章 1 次身份解锁"), ("群像", "女性角色 ≥ 2/3")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        rules: &[
            "零恋爱线；第一章必须完成一次对剧情原设的主动改写（抢先机/破坏关键节点）",
            "每条机缘配一个明确的战力或资源增量，用独立段落呈现",
            "每 10 章至少 1 次「称号/身份」解锁；章节标题可用 & 并列多个信息点",
            "女主不作台前英雄：隐藏身份、背后搞事；前期与官方互惠互利",
            "女性角色占比 2/3 以上且各有事业线，禁止雌竞；反派不降智",
            "主角价值观：爱自己、不无私奉献，但不对无辜者出手",
        ],
        avoid: &["恋爱脑 / 雌竞 / 降智反派", "喊口号式大女主（制度层面不兑现）", "圣母式救世"],
        opening: "重生或开局第一件事是破坏仇人的关键节点，完成一次主动改写",
        ending: "结算时称号列表又长了一条，或揭示下一个可抢的机缘",
    },
    StylePreset {
        id: "lianai",
        name: "现代言情·拉扯甜虐",
        group: "女频",
        tagline: "卖拉扯感而不是平淡甜宠，克制且落到实处",
        genres: "现言 / 都市情感 / 追妻火葬场 / 甜虐",
        traits: &[("拉扯", "每章 1 次进退张力"), ("对话", "≥ 50% 情绪表达"), ("配比", "甜 6 虐 4")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: Some(0.45), max_sentence_len: None, forbid_modern_words: false },
        rules: &[
            "感情推进落到具体日常互动（修水管/顺路送伞/深夜加班），禁止靠物质铺张示好",
            "每章至少 1 次「靠近一步又退半步」的张力设计",
            "男主必须始终专一（道德洁癖型），前期不得突破读者接受的底线",
            "对话承担 50% 以上情绪表达；禁止大段内心独白式抒情",
            "章末落在情绪钩子上（未说出口的话/被打断的动作/意外的目击）",
            "甜虐配比 6 比 4；虐必须服务于关系升级，而不是折磨读者",
        ],
        avoid: &["男主前期渣到无法挽回（追妻火葬场大忌）", "误会靠不沟通硬撑超过 3 章", "雌竞工具女配"],
        opening: "关系的既有状态加一次意外打破（重逢/契约/误认），第一场戏要有具体肢体或场景接触",
        ending: "一句没说出口的话，或一个被打断的动作",
    },
    StylePreset {
        id: "gufeng",
        name: "古风雅致·家宅权谋",
        group: "女频",
        tagline: "以时代礼制为牢笼，用器物与称谓的精确换可信度",
        genres: "古代言情 / 宅斗 / 宫斗 / 权谋 / 大女主",
        traits: &[("章名", "2 字为主"), ("考据", "每 800 字 1 处器物/礼制"), ("情绪", "含蓄外化")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: true },
        rules: &[
            "章名 2 字为主（归来/门户/毒死），章名本身就是钩子",
            "每 800 字至少 1 处具体的器物、礼制、节令或称谓细节",
            "严禁现代词（情绪价值、破防、内耗一类）；称谓、礼制、时序不能出错",
            "情绪不直说：用合乎礼法却反常的举动代替「她很愤怒」",
            "反派不降智：每次陷害都合乎时代规则，主角靠对规则的理解破局",
            "男主是变量不是答案；女主的转折必须由自己的选择触发",
            "允许慢热，但每章必须有一次小博弈",
        ],
        avoid: &["穿越女主呼风唤雨、把时代工具化", "爱情至上叙事", "现代三观直接投射到古代语境"],
        opening: "一个合乎礼法的日常场景里出现不可逆的变故（死讯/回京/赐婚）",
        ending: "请安或家宴结束，最后一行是某人的袖子下藏着一封信",
    },
];

/// 按 id 查找预设；id 为空或未知名返回 None（保持既有默认写法）
pub fn find(id: &str) -> Option<&'static StylePreset> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    PRESETS.iter().find(|p| p.id == id)
}

/// 渲染成提示词块（平台基线 + 风格规则 + 反例 + 开场/章末参考）
pub fn style_block(p: &StylePreset) -> String {
    let mut s = String::new();
    s.push_str(&format!("【文风预设：{} —— {}】\n", p.name, p.tagline));
    s.push_str("平台节奏基线：\n");
    for b in PLATFORM_BASELINE {
        s.push_str(&format!("- {}\n", b.replace("{words}", p.word_target)));
    }
    s.push_str("风格规则：\n");
    for r in p.rules {
        s.push_str(&format!("- {r}\n"));
    }
    s.push_str("反例（出现即视为不合格）：\n");
    for a in p.avoid {
        s.push_str(&format!("- {a}\n"));
    }
    s.push_str(&format!("开场参考：{}\n", p.opening));
    s.push_str(&format!("章末参考：{}", p.ending));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_consistent() {
        assert!(PRESETS.len() >= 8, "预设数量太少: {}", PRESETS.len());
        let mut ids: Vec<&str> = PRESETS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "预设 id 必须唯一");
        for p in PRESETS {
            assert!(!p.name.is_empty() && !p.tagline.is_empty(), "{} 缺少名称/定位", p.id);
            assert!(!p.rules.is_empty(), "{} 没有规则", p.id);
            assert!(!p.avoid.is_empty(), "{} 没有反例", p.id);
            assert!(p.word_target.contains("字"), "{} 字数窗口异常", p.id);
        }
    }

    #[test]
    fn group_values_are_known() {
        for p in PRESETS {
            assert!(
                ["男频", "女频", "通用"].contains(&p.group),
                "{} 的频道值非法: {}",
                p.id,
                p.group
            );
        }
    }

    #[test]
    fn checks_field_is_wired() {
        assert!(
            PRESETS.iter().any(|p| p.checks.min_dialogue_ratio.is_some()),
            "至少一个预设应有对话占比要求"
        );
        assert!(
            PRESETS.iter().filter(|p| p.checks.forbid_modern_words).count() >= 2,
            "古风/纪实应禁现代词"
        );
        assert_eq!(find("shadiao").unwrap().checks.max_sentence_len, Some(45));
        for p in PRESETS {
            if let Some(r) = p.checks.min_dialogue_ratio {
                assert!((0.1..=0.9).contains(&r), "{} 对话占比阈值不合理", p.id);
            }
        }
    }

    #[test]
    fn find_handles_unknown_and_empty() {
        assert!(find("").is_none());
        assert!(find("   ").is_none());
        assert!(find("不存在").is_none());
        assert_eq!(find("xuanyi").map(|p| p.id), Some("xuanyi"));
        assert_eq!(find("  rexue  ").map(|p| p.id), Some("rexue"));
    }

    #[test]
    fn style_block_contains_rules_and_baseline() {
        let p = find("gufeng").unwrap();
        let block = style_block(p);
        assert!(block.contains("古风雅致"), "块内应含预设名");
        assert!(block.contains("章名 2 字为主"), "块内应含风格规则");
        assert!(block.contains("平台节奏基线"), "块内应含平台基线");
        assert!(block.contains(p.word_target), "基线应替换为风格字数窗口");
        assert!(block.contains("反例"));
    }

    #[test]
    fn style_block_never_breaks_mock_markers() {
        // Mock 提供方以「=`、`TASK:`、「想法：」等标记解析 user 文本；
        // 风格块是追加内容，必须保持干净，否则会污染离线演示与测试。
        for p in PRESETS {
            let block = style_block(p);
            for bad in ["=", "TASK:", "想法：", "原段落：", "chapter_index"] {
                assert!(!block.contains(bad), "{} 的风格块包含 Mock 标记 `{bad}`", p.id);
            }
        }
    }
}
