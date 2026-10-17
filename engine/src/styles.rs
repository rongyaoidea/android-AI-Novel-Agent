//! 写作风格预设：基于番茄小说生态调研蒸馏（见 docs/风格调研-番茄.md）。
//!
//! 设计原则：
//! - 预设以「风格类型」命名，不以作者命名（规避版权与模仿风险）；
//! - 每条规则必须**可执行、可自检**（量化字数/占比/频率），不写「文采好」这类空话；
//! - 选中预设时，平台通用节奏基线与风格规则一起注入生成提示词；未选预设保持原有行为。
//!
//! 注意：注入文本中禁止出现 `=`、`TASK:`、`想法：` 等 Mock 提供方用于解析的标记。

use serde::Serialize;

use crate::style_fingerprint::FingerprintBand;

/// const 构造辅助（用于静态指纹带字面量）
const fn band(metric: &'static str, min: f64, max: f64) -> FingerprintBand {
    FingerprintBand { metric, min, max }
}

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
    /// 风格指纹带（S1：生成后对照告警；工程参考带，可调）
    pub bands: &'static [FingerprintBand],
    /// 风格锚定样本（S3：原创短文，注入提示词做语感锚定；禁止照抄）
    pub samples: &'static [&'static str],
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
        bands: &[
            band("avg_para_len", 16.0, 40.0),
            band("avg_sentence_len", 11.0, 24.0),
            band("short_sentence_ratio", 0.25, 0.55),
            band("quote_ratio", 0.05, 0.28),
            band("exclaim_per_k", 0.0, 2.5),
            band("ellipsis_per_k", 0.3, 4.5),
            band("idiom_per_k", 0.0, 2.5),
        ],
        samples: &[
            "走廊尽头那扇门虚掩着，门缝里没有光。他数着自己的心跳走过去，数到第七下时，门后传来一声很轻的响动——像有人把呼吸咽了回去。他没有回头。他知道，回头的人从来没能走到最后。",
            "规则第一条：听见敲击声，不要应答。他盯着墙上那行褪色的字，指节泛白。昨夜，隔壁的人应答了。今早，隔壁的床铺平整得像从来没有人睡过，只有枕头上压着一张纸，上面写着他自己的名字。",
        ],
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
        bands: &[
            band("avg_para_len", 13.0, 34.0),
            band("avg_sentence_len", 9.0, 22.0),
            band("short_sentence_ratio", 0.3, 0.62),
            band("quote_ratio", 0.12, 0.4),
            band("exclaim_per_k", 1.0, 6.0),
            band("ellipsis_per_k", 0.0, 3.0),
            band("idiom_per_k", 0.0, 2.5),
        ],
        samples: &[
            "“退后。”他吐出一口血沫，把少年护在身后。刀光再起，他迎上去。一步，两步，第三步时刀断了——他就用断刀。身后的少年喊他的名字，他没应。有些名字，要等到活下来才配被叫。",
            "风雪压城，城头只剩七个人。最年轻的那个问：我们守得住吗？最老的那个笑了笑，把最后一壶酒浇在旗杆下。守不守得住是明天的事，今天的事是——旗，不能倒。",
        ],
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
        bands: &[
            band("avg_para_len", 11.0, 28.0),
            band("avg_sentence_len", 8.0, 18.0),
            band("short_sentence_ratio", 0.35, 0.68),
            band("quote_ratio", 0.28, 0.62),
            band("exclaim_per_k", 1.0, 6.5),
            band("ellipsis_per_k", 0.0, 2.5),
            band("idiom_per_k", 0.0, 1.5),
        ],
        samples: &[
            "【叮！签到成功，获得‘老城区商铺三间’。】他捏着手机站在公交站台，雨水顺着伞骨往下淌。三间商铺，按现在的市价——他心算了一遍，又算了一遍。旁边的大爷问他怎么了，他说没事，就是突然不冷了。",
            "系统面板上跳出一行字：【今日打脸额度：1/1】。他把手机塞回兜里，抬头看向会议室。对面的人还在念稿子，念的是怎么把他踢出局。他忽然有点想笑——这稿子，怕是白念了。",
        ],
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
        bands: &[
            band("avg_para_len", 11.0, 30.0),
            band("avg_sentence_len", 9.0, 20.0),
            band("short_sentence_ratio", 0.3, 0.62),
            band("quote_ratio", 0.18, 0.48),
            band("exclaim_per_k", 1.0, 6.0),
            band("ellipsis_per_k", 0.0, 2.5),
            band("idiom_per_k", 0.0, 2.5),
        ],
        samples: &[
            "“废材也配上台？”哄笑声里，他慢慢抬起头。丹田处那道被判定为死脉的旧伤，此刻正一跳一跳地发烫。他没有说话，只是把手按在了测试石上。石头亮起的一瞬间，全场安静得能听见风。",
            "三年前，他们拆了他的屋，烧了他的书。三年后他站在山门口，守门弟子问他名号。他想了想，报了自己从前的名字。守门弟子脸色变了——那个名字，族谱上早就划掉了。",
        ],
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
        bands: &[
            band("avg_para_len", 15.0, 36.0),
            band("avg_sentence_len", 11.0, 24.0),
            band("short_sentence_ratio", 0.22, 0.52),
            band("quote_ratio", 0.12, 0.4),
            band("exclaim_per_k", 0.3, 3.0),
            band("ellipsis_per_k", 0.0, 3.5),
            band("idiom_per_k", 0.0, 2.5),
        ],
        samples: &[
            "春日午后，他把晒了三天的药材收回竹匾，顺手给院里的老桃树浇了水。邻村传来消息说，山下又有大人物打起来了。他嗯了一声，把火调小，锅里的粥咕嘟咕嘟冒着泡。活得久的人，都懂得什么时候不看热闹。",
            "第两百个年头，他学会了新的法术。法术不强，胜在省力。徒弟问他为什么不学更厉害的那种，他指了指后山的坟——那些学厉害的，都躺下了。他还要再活几百年，不急。",
        ],
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
        bands: &[
            band("avg_para_len", 17.0, 40.0),
            band("avg_sentence_len", 12.0, 26.0),
            band("short_sentence_ratio", 0.18, 0.48),
            band("quote_ratio", 0.05, 0.22),
            band("exclaim_per_k", 0.0, 1.8),
            band("ellipsis_per_k", 0.0, 2.5),
            band("idiom_per_k", 0.0, 2.8),
        ],
        samples: &[
            "我是跟着师父进的山。那年他五十三，背有点驼，手里拎着一盏气死风灯。进洞前他回头跟我说了一句话，我记了二十年：里头的东西，认得路，不认人。",
            "东西是从河滩上捞上来的，铜的，锈得看不出纹样。我用指甲盖刮了刮，刮出一线亮色。师父凑过来看了一眼，脸色就变了，让我原样放回去，磕头，退三步。我照做了。当天夜里，河滩上多了一串脚印，从水里一直走到我们帐篷门口。",
        ],
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
        bands: &[
            band("avg_para_len", 9.0, 24.0),
            band("avg_sentence_len", 7.0, 16.0),
            band("short_sentence_ratio", 0.4, 0.72),
            band("quote_ratio", 0.38, 0.68),
            band("exclaim_per_k", 1.0, 6.5),
            band("ellipsis_per_k", 0.0, 2.5),
            band("idiom_per_k", 0.0, 1.2),
        ],
        samples: &[
            "“您这演技，”她把话筒往台上一放，“放在博物馆都得单独开个展柜，标签写：人类早期尴尬现场。”台下安静了两秒，然后炸了。直播间的弹幕已经刷疯了——她要的就是这两秒安静。",
            "热搜第三条是她。她慢悠悠地转发，配了一句：恭喜我，又免费上了一次班。评论区笑作一团，对家的公关团队连夜开了三个会。她关了手机，敷面膜，睡觉。",
        ],
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
        bands: &[
            band("avg_para_len", 12.0, 30.0),
            band("avg_sentence_len", 9.0, 20.0),
            band("short_sentence_ratio", 0.3, 0.62),
            band("quote_ratio", 0.12, 0.42),
            band("exclaim_per_k", 0.3, 3.5),
            band("ellipsis_per_k", 0.0, 2.5),
            band("idiom_per_k", 0.0, 1.8),
        ],
        samples: &[
            "副本开始的第三分钟，她已经摸清了机制。别人还在讨论怎么保命，她绕到后山，把刷新点堵了。系统提示跳出来的时候，她正在清点战利品。队友问她怎么知道这里——她没答。上一世，她就是死在这里的。",
            "结算面板亮起：【称号解锁：夜行者】。她把称号戴上，属性涨了一截。不远处，曾经看不起她的队长正被小怪追得满地跑。她看了一眼，转身走了——救他，不划算。",
        ],
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
        bands: &[
            band("avg_para_len", 11.0, 30.0),
            band("avg_sentence_len", 8.0, 18.0),
            band("short_sentence_ratio", 0.3, 0.62),
            band("quote_ratio", 0.32, 0.62),
            band("exclaim_per_k", 0.3, 3.5),
            band("ellipsis_per_k", 0.3, 4.5),
            band("idiom_per_k", 0.0, 1.8),
        ],
        samples: &[
            "他把伞往她那边倾了倾，自己的肩膀湿了一片。她看见了，没说话。走到楼下时，雨停了。他收起伞，忽然说：明天也下雨的话——话没说完，她转身进了楼道。他没看见的是，她在二楼窗口站了很久。",
            "深夜十一点，她的工位还亮着灯。保温桶放在桌角，是她早上带来的，里面是热过的汤。便签上只有四个字：趁热喝。她盯着那四个字看了很久，忽然发现，自己已经很久没跟人说过谢谢了。",
        ],
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
        bands: &[
            band("avg_para_len", 19.0, 45.0),
            band("avg_sentence_len", 13.0, 30.0),
            band("short_sentence_ratio", 0.12, 0.42),
            band("quote_ratio", 0.08, 0.32),
            band("exclaim_per_k", 0.0, 2.0),
            band("ellipsis_per_k", 0.3, 4.5),
            band("idiom_per_k", 1.0, 8.0),
        ],
        samples: &[
            "晨起请安，她立在廊下，听里头的笑声一阵高过一阵。贴身丫鬟急得攥紧了帕子，她反倒替人理了理被风吹乱的披风系带，声音低而稳：“急什么。天要变，先看云往哪边走。”",
            "席间敬酒，她推说身子不适，只以茶代酒。无人留意，她袖中的手在桌下把一张字条捻成了灰。散席时经过假山，她脚步未停，声音轻得像自语：二更，后角门。身后，一个影子无声地退进了夜色里。",
        ],
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
    // ---------- 2026-10 调研扩容（S2：覆盖起点/晋江/番茄调性，去名化命名） ----------
    StylePreset {
        id: "shishi",
        name: "史诗玄幻·恢宏意象",
        group: "男频",
        tagline: "用意象与排比造势，写一个时代而不是一个人",
        genres: "东方玄幻 / 史诗战争 / 洪荒神话 / 群像",
        traits: &[("句式", "排比式短句造势"), ("意象", "密度高于辞藻"), ("钩子", "设定/境界投放")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        bands: &[
            band("avg_para_len", 20.0, 50.0),
            band("avg_sentence_len", 13.0, 30.0),
            band("short_sentence_ratio", 0.12, 0.42),
            band("quote_ratio", 0.05, 0.22),
            band("exclaim_per_k", 0.3, 3.0),
            band("ellipsis_per_k", 0.3, 4.5),
            band("idiom_per_k", 1.0, 6.0),
        ],
        samples: &[
            "那是人族最坏的时代，也是最好的时代。城破了，旗倒了，可有人把断旗重新插回了城头。风从荒原上吹来，卷起沙尘，也卷起亿万人的名字——他们不曾留名，却撑起了这片天。",
            "他站在山巅，看着云海翻涌。师门覆灭那日，他十六岁；今日他二十八，身后是九座墓碑，身前是百万敌军。他缓缓拔剑。剑鸣如龙，天地失色。",
        ],
        rules: &[
            "大事件用排比式短句造势（三段以内），日常用中长句推进；排比不滥用",
            "意象密度优先于辞藻：用具体物象（断旗/风沙/剑鸣）承载情绪，禁止抽象形容词堆砌",
            "章末用「设定/境界/身世」投放做钩子，但同一设定名不得连续两章投放",
            "每个配角至少一次立场抉择；群像不写成工具人",
            "战斗写代价与后果，不写数值碾压流水账",
        ],
        avoid: &["空喊口号式宏大（无具体物象支撑）", "场景描写同词重复（落叶/秋风连用）", "全员陪衬主角"],
        opening: "以时代切片开场（城破/灾变/登临），第一章内让主角与某个大设定发生绑定",
        ending: "断旗重立的画面里，镜头拉远，露出更远处的另一座烽火",
    },
    StylePreset {
        id: "kesu",
        name: "克苏鲁悬疑·多线群像",
        group: "男频",
        tagline: "冷峻考据 + 多线并行，靠解谜推进而不是靠爽点",
        genres: "克苏鲁 / 悬疑解谜 / 蒸汽幻想 / 都市怪谈",
        traits: &[("视角", "多线并行群像"), ("推进", "解谜而非爽点"), ("情绪", "克制，行为外化")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        bands: &[
            band("avg_para_len", 18.0, 45.0),
            band("avg_sentence_len", 12.0, 28.0),
            band("short_sentence_ratio", 0.18, 0.48),
            band("quote_ratio", 0.08, 0.32),
            band("exclaim_per_k", 0.0, 2.0),
            band("ellipsis_per_k", 0.3, 4.5),
            band("idiom_per_k", 0.3, 4.0),
        ],
        samples: &[
            "第三次翻阅档案时，他注意到一个细节：所有失踪者的登记照里，背景墙上都挂着一幅画——同一幅，连挂歪的角度都一样。他合上卷宗，手指在桌面上敲了两下。这不是巧合，这是规律。而规律，是用来利用的。",
            "仪式定在子夜。实验室的钟停了三天，秒针死死钉在十一点五十九分。记录员在日志上写下最后一行字：声音从墙里传出来时，请务必不要回应。然后他合上本子，把钢笔插回口袋，像赴一场普通的会议。",
        ],
        rules: &[
            "多线并行：每章至少切换一次视角，各线信息互补但不重复交代",
            "细节考据优先：每章至少 1 个可查证的时代/行业细节做真实感锚点",
            "情绪克制：用行为表现心理（动作、停顿、记录），禁止大段内心独白",
            "每章推进 1 个谜题环节并留下 1 个未解事实；答案必须能被读者回溯验证",
            "恐怖来自「规律」与「代价」，不靠血腥描写堆砌",
        ],
        avoid: &["上帝视角提前剧透真相", "用幻觉/做梦结算剧情", "连续两章无信息增量"],
        opening: "一桩有「规律感」的异常事件开场，主角以专业身份介入",
        ending: "某个早已出现的细节被重新解释，读者的安全感被推翻一半",
    },
    StylePreset {
        id: "jiazu",
        name: "家族群像·文白修仙",
        group: "男频",
        tagline: "文白相杂的古典简练，写家族的兴衰与生死",
        genres: "家族修仙 / 种田崛起 / 古典仙侠 / 群像",
        traits: &[("语言", "文白相杂·简练"), ("视角", "全知+旁观"), ("结构", "世代更替")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        bands: &[
            band("avg_para_len", 17.0, 40.0),
            band("avg_sentence_len", 11.0, 26.0),
            band("short_sentence_ratio", 0.18, 0.48),
            band("quote_ratio", 0.08, 0.3),
            band("exclaim_per_k", 0.0, 1.8),
            band("ellipsis_per_k", 0.3, 4.5),
            band("idiom_per_k", 1.0, 7.0),
        ],
        samples: &[
            "族老们跪在祠堂外，雪落了一夜。少年的灵位摆在最末，名字写得小，却一笔一划不曾潦草。有人低声说他死得不值，老人只是抬头看了看牌匾，淡淡道：宗门立了千年，靠的不是值不值，是有人肯去。",
            "三更，山门钟响。他披衣而起，案上茶未凉。信中只八字：北境有变，速援。他提剑出门，回望妻儿睡处，未作停留。此去山高水远，他自知未必能归。",
        ],
        rules: &[
            "文白相杂：文言虚词与单音节动词适量（信/归/未/矣），整体仍是现代阅读节奏",
            "全知视角 + 旁观者视角（族谱/镜灵/史官口吻）交替，主角可隐于幕后",
            "死亡是叙事工具：重要角色之死必须完成其人物弧（最后一块拼图），但不滥用",
            "世代推进：每卷至少一次时间跨度（数年），用家族变化体现",
            "每章一个具体事务（婚丧/授业/查账/出征）承载主线",
        ],
        avoid: &["角色匆匆死亡只为煽情", "全员长生不死无代价", "现代网络词"],
        opening: "以家族事务（祭祀/分家/出征）开场，透露家族正在走下坡路",
        ending: "一件旧物或一句旧话被重新提起，指向多年前埋下的人",
    },
    StylePreset {
        id: "minsu",
        name: "民俗怪谈·行业诡事",
        group: "男频",
        tagline: "行业规矩即世界观，禁忌与代价都要写清",
        genres: "民俗怪谈 / 捞尸盗墓 / 江湖规矩 / 都市诡事",
        traits: &[("词汇", "行业词密集"), ("钩子", "案件式章节名"), ("规矩", "三不捞式禁忌")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        bands: &[
            band("avg_para_len", 14.0, 35.0),
            band("avg_sentence_len", 10.0, 22.0),
            band("short_sentence_ratio", 0.22, 0.55),
            band("quote_ratio", 0.12, 0.42),
            band("exclaim_per_k", 0.3, 3.5),
            band("ellipsis_per_k", 0.5, 5.5),
            band("idiom_per_k", 0.3, 3.5),
        ],
        samples: &[
            "干我们这行的，有三不捞：子时的水不捞，漂着的不捞，喊你名字的不捞。我师父破过第二条，捞上来的东西在岸边坐了一宿。天亮时它走了，河滩上留了两行脚印——只有进来的，没有出去的。",
            "接亲的队伍走的是水路。船头挂白灯笼，船尾压红布包，据说是给「那边」看的。船到江心，鼓乐停了一瞬——就那一瞬，我听见水底下有人跟着唱。老船工脸色不变，往江里撒了一把米。歌声，停了。",
        ],
        rules: &[
            "行业规矩即世界观：禁忌写死、代价写清，破了规矩必须付账",
            "行业黑话首次出现自然带出含义（借人物口吻解释），不注释不掉书袋",
            "章节名用案件式短语（「点灯走江」「认祖归宗」），名即钩子",
            "每个怪事给一条可验证的「规矩」，读者能提前猜、事后能对上",
            "第一人称或近距离第三人称为主，语气平实，允许自嘲",
        ],
        avoid: &["怪事无规则（想怎么编怎么编）", "主角零风险破局", "恐怖词库反复使用"],
        opening: "以一条行业禁忌开场，立刻打破或险些打破它",
        ending: "章末留下一件「已经发生但没人承认」的事",
    },
    StylePreset {
        id: "lishi",
        name: "历史权谋·庙堂江湖",
        group: "男频",
        tagline: "朝会上的话只听三分，散了朝才是真正的较量",
        genres: "架空历史 / 庙堂权谋 / 武侠江湖 / 争霸",
        traits: &[("对话", "双层意思"), ("节奏", "每章一次博弈"), ("落点", "利益而非立场")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        bands: &[
            band("avg_para_len", 18.0, 45.0),
            band("avg_sentence_len", 12.0, 28.0),
            band("short_sentence_ratio", 0.15, 0.45),
            band("quote_ratio", 0.18, 0.48),
            band("exclaim_per_k", 0.0, 2.0),
            band("ellipsis_per_k", 0.3, 4.5),
            band("idiom_per_k", 1.0, 5.5),
        ],
        samples: &[
            "“你要查的案子，我劝你到此为止。”茶盏推过来，水汽氤氲。他端起来抿了一口，笑道：“大人这话，若是半年前说，我信。”对面的人盯着他看了很久，终于叹了口气：“明日寅时，西角门，我送一个人给你。”",
            "朝会上争的是漕运，散了朝，真正的较量才开始。他回到值房，把那封密信在灯上燎了，灰烬收进荷包——要交给谁，他还没想好。棋盘上落子容易，收子难。",
        ],
        rules: &[
            "对话必有第二层意思：每场关键对白至少一句「话里有话」，事后可回读",
            "每章一次小博弈（试探/交换/退让），落点写利益结构而不是立场口号",
            "官职、称谓、礼制不出错；不确定的细节宁可模糊处理",
            "谋略要能被读者复盘：信息差、把柄、时间窗口三要素至少写清两个",
            "反派不降智：对手的每一步都要有合理动机",
        ],
        avoid: &["主角开挂式算无遗策（对手集体降智）", "大段历史科普", "现代观念直接投射"],
        opening: "一场看似寻常的茶叙或朝会开场，第一句话就藏着试探",
        ending: "散场之后，某人做了一件与刚才表态相反的事",
    },
    StylePreset {
        id: "wanggan",
        name: "网感修真·梗驱动",
        group: "男频",
        tagline: "当代语汇解构修真，梗要服务剧情不是堆砌",
        genres: "网感修仙 / 系统流 / 搞笑脑洞 / 都市修真",
        traits: &[("语汇", "当代梗+修真术语混搭"), ("句式", "极短句"), ("节奏", "每章 1 个可传播梗")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        bands: &[
            band("avg_para_len", 9.0, 25.0),
            band("avg_sentence_len", 7.0, 17.0),
            band("short_sentence_ratio", 0.4, 0.72),
            band("quote_ratio", 0.28, 0.58),
            band("exclaim_per_k", 1.0, 6.5),
            band("ellipsis_per_k", 0.0, 2.5),
            band("idiom_per_k", 0.0, 1.2),
        ],
        samples: &[
            "【叮！检测到宿主怨气值超标，已自动为您开启‘嘴强王者’天赋。】他看着眼前这行字，沉默了三秒。行吧。从今天起，谁惹他，他就骂谁，骂得越狠修为涨得越快——修真界，终究是颠了。",
            "“师兄，境界不够怎么办？”小师妹眼巴巴地问。他掏出一张符，拍在她脑门上：“借的，年化率三点五，渡劫前还清。”师妹：“？？”山风拂过，他眺望远方——这个修真界，迟早被自己盘活。",
        ],
        rules: &[
            "梗必须服务剧情：每个梗要么推进信息、要么塑造人物，禁止纯堆砌",
            "修真术语与现代语汇混搭要有「规则感」（如法力贷/年化率），且全书自洽",
            "极短句为主，金句独立成段；每章至少 1 个可截图的梗",
            "搞笑之下保留一条认真的主线目标，不许全员发疯",
            "战斗与升级写「离谱但合理」的解法，不写数值流水账",
        ],
        avoid: &["梗与剧情无关的段子合集", "全员降智只为搞笑", "热梗过载（每段都是梗）"],
        opening: "系统/天赋以吐槽方式登场，第一屏内完成一次「离谱但合理」的展开",
        ending: "一句一本正经的胡说八道，把下一章的期待值拉满",
    },
    StylePreset {
        id: "jinglian",
        name: "晋江精练·尖诮幽默",
        group: "女频",
        tagline: "嬉笑尖诮地写苦难，对白像相声但刀是真刀",
        genres: "刑侦 / 武侠 / 科幻 / 群像 / 强强",
        traits: &[("语言", "精练+黑色幽默"), ("对白", "相声式交锋"), ("情绪", "笑里藏刀")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        bands: &[
            band("avg_para_len", 12.0, 32.0),
            band("avg_sentence_len", 10.0, 22.0),
            band("short_sentence_ratio", 0.25, 0.58),
            band("quote_ratio", 0.28, 0.58),
            band("exclaim_per_k", 0.3, 3.0),
            band("ellipsis_per_k", 0.3, 4.5),
            band("idiom_per_k", 0.3, 3.0),
        ],
        samples: &[
            "“你这个人，”他慢条斯理地把刀擦干净，“最大的优点是坦诚，最大的缺点是坦诚得过于准时。”对面的人沉默了。刀归鞘，他起身，掸了掸衣角，像刚参加完一场无聊的茶会——如果忽略地上那三具「茶客」的话。",
            "世人说他心狠，他承认。世人说他嘴毒，他也承认。世人说他不近人情——这就冤枉了。他分明很讲道理，只不过他的道理，一般活不过三句对话。",
        ],
        rules: &[
            "语言精练：能删的字都删；每段至少一处信息增量",
            "对白像相声：一来一回有节奏，但每场交锋底下压着真实的利害",
            "黑色幽默写苦难：越沉重的场景，语气越轻（反差），禁止煽情直给",
            "人物再惨也保留体面与机锋；禁止卖惨式哭戏",
            "案件/冲突逻辑要经得起复盘，幽默不牺牲严密",
        ],
        avoid: &["为幽默牺牲逻辑", "段子式对白脱离剧情", "全员插科打诨无正剧时刻"],
        opening: "一场看似轻松的对话开场，三句内露出底下的刀",
        ending: "一句玩笑话落地，读者却笑不出来",
    },
    StylePreset {
        id: "rouguang",
        name: "柔光抒情·治愈短句",
        group: "女频",
        tagline: "把遗憾写得很轻，把温柔写得很具体",
        genres: "青春 / 治愈 / 日常 / 群像情感",
        traits: &[("句式", "短句抒情"), ("落点", "具体物件承载情绪"), ("收束", "柔光式短句")],
        word_target: "1800~2200 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        bands: &[
            band("avg_para_len", 11.0, 30.0),
            band("avg_sentence_len", 9.0, 20.0),
            band("short_sentence_ratio", 0.3, 0.62),
            band("quote_ratio", 0.12, 0.42),
            band("exclaim_per_k", 0.0, 1.8),
            band("ellipsis_per_k", 0.5, 6.0),
            band("idiom_per_k", 0.0, 2.0),
        ],
        samples: &[
            "很多年以后他还记得那个傍晚。蝉声把黄昏拉得很长，她站在巷口朝他挥手，白衬衫被风吹得鼓起来。他喊了一声慢点跑，她没听见。后来他走了很远的路，再没见过那样亮的晚霞。",
            "冬天来的时候，老屋的窗上结了霜花。她用手指画了个圈，圈里是她和一只猫。猫已经老了，趴在炉边打盹。她把毯子分它一半，轻声说：再陪我一年吧。炉火噼啪响了一声，像谁在答应。",
        ],
        rules: &[
            "情绪用具体物件与动作承载（霜花/保温桶/糖纸），禁止直接写「很难过/很温暖」",
            "短句为主，抒情段落允许一句独立成段；每章至少一处「柔光收束」",
            "温柔要具体：写明谁为谁做了什么小事，不写抽象的「被治愈」",
            "遗憾与失去写得轻：留白多于铺陈，禁止狗血冲突",
            "时间感清晰（季节/年份锚点），让回忆有重量",
        ],
        avoid: &["强行煽情（每段都在抒情）", "剧情全靠误会推进", "心灵鸡汤式总结"],
        opening: "以一个有年代感的日常物件或场景开场，埋下一段将要说再见的时光",
        ending: "一件小事安静地完成，读者自己读出那句没说出口的话",
    },
    StylePreset {
        id: "jingwei",
        name: "古言京味·世家日常",
        group: "女频",
        tagline: "嘴上说的和心里想的隔着三层院子",
        genres: "古言 / 世家日常 / 宅斗轻喜剧 / 京味",
        traits: &[("语言", "京味儿幽默+红楼意韵"), ("博弈", "闲话里见刀"), ("考据", "器物/礼制精确")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: true },
        bands: &[
            band("avg_para_len", 17.0, 42.0),
            band("avg_sentence_len", 11.0, 26.0),
            band("short_sentence_ratio", 0.18, 0.48),
            band("quote_ratio", 0.22, 0.52),
            band("exclaim_per_k", 0.0, 2.2),
            band("ellipsis_per_k", 0.3, 4.5),
            band("idiom_per_k", 1.0, 6.0),
        ],
        samples: &[
            "“哟，这是打哪儿来的一身寒气？”老太太盘腿坐在炕上，手里盘着核桃，眼皮都没抬。她规规矩矩行了礼，把带来的点心搁在炕桌上。老太太这才瞥了一眼：“这匣子点心，铺子里卖八两，你买了十两——冤大头也不是这么当的。”",
            "府里上下几百口人，嘴上说的和心里想的隔着三层院子。她进门三个月，学会了把话听半句、把事看七分。晚饭后她陪着老太太遛弯，老太太忽然问：“你瞧这院子，哪处最干净？”她答：“账房。”老太太乐了：“丫头，有你的。”",
        ],
        rules: &[
            "京味儿对白：语气词与闲话要有生活气（哟/得嘞/您呐），但不油滑",
            "闲话里见刀：每次家常对话至少藏一层试探或交换",
            "器物、礼制、称谓精确；节令与饮食习惯可作日常戏的骨架",
            "情绪含蓄外化：写「合乎礼法的反常举动」，不直写喜怒",
            "老太太/长辈是信息中枢：重要情报通过饭桌闲谈投放",
        ],
        avoid: &["现代词与现代观念", "宅斗全靠下毒陷害（太低端）", "对白沦为纯唠嗑无信息"],
        opening: "一顿家常饭或一次请安开场，席间一句话点出府里的暗流",
        ending: "散场后某件小物（点心匣/核桃/账本）被赋予新含义",
    },
    StylePreset {
        id: "niandai",
        name: "年代治愈·行业温情",
        group: "男频",
        tagline: "用年代的细节与行业的烟火气，写普通人的上升",
        genres: "年代文 / 行业文 / 美食经营 / 家长里短",
        traits: &[("细节", "年代词纪律"), ("爽点", "小事业步步兑现"), ("基调", "温暖不苦情")],
        word_target: "2000~2400 字",
        checks: StyleChecks { min_dialogue_ratio: None, max_sentence_len: None, forbid_modern_words: false },
        bands: &[
            band("avg_para_len", 15.0, 38.0),
            band("avg_sentence_len", 11.0, 24.0),
            band("short_sentence_ratio", 0.22, 0.52),
            band("quote_ratio", 0.12, 0.42),
            band("exclaim_per_k", 0.0, 2.5),
            band("ellipsis_per_k", 0.3, 4.5),
            band("idiom_per_k", 0.3, 3.5),
        ],
        samples: &[
            "灶上的水开了，白汽顶得锅盖哐哐响。她把面下进去，又卧了个鸡蛋。窗外，厂里的广播正放《年轻的朋友来相会》，自行车铃铛一路响进巷子。她擦了擦手，朝门口喊了一声：洗手，吃饭！",
            "八十年代的供销社，糖是按两称的。他攒了三个月的糖票，换了二两水果糖，用手帕包着塞给她。她没舍得吃，锁进了木箱。多年后搬家翻出那只箱子，糖化了，纸还包着，一打开，满屋子都是甜味。",
        ],
        rules: &[
            "年代词纪律：物品、票据、称谓、口号都要对得上年代；不确定的细节宁可模糊",
            "行业细节要真：做饭/修表/跑运输的手艺与门道写出可感步骤",
            "爽点是小事业步步兑现（第一笔收入/第一个回头客），不写暴富",
            "人物关系温暖但不苦情：矛盾来自时代与性格，不来自恶意陷害",
            "每章至少一处烟火气细节（饭菜/天气/街坊），与主线挂钩",
        ],
        avoid: &["年代错乱的物品与词汇", "开挂式暴富", "全员恶人式家长里短"],
        opening: "以一段有年代感的生活场景开场，主角的「手艺」在第一屏亮相",
        ending: "小生意或小日子有了一个新起色，落在一件具体的小物上",
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

/// 运行时风格规格：把「静态预设」与「用户自定义风格」统一到同一层，
/// 供生成 / 本地检查 / 指纹对照使用（预设为静态数据，自定义来自数据库）。
#[derive(Debug, Clone)]
pub struct StyleSpec {
    pub id: String,
    pub name: String,
    pub tagline: String,
    pub word_target: String,
    /// 完整注入块（预设含平台基线；自定义为卡片文本）
    pub block: String,
    /// 风格锚定样本（每章轮换注入一段）
    pub samples: Vec<String>,
    /// 写作规则（自定义风格用于 review 提示等场景）
    pub rules: Vec<String>,
    /// 反例清单
    pub avoid: Vec<String>,
    /// 指纹带
    pub bands: Vec<FingerprintBand>,
    /// 本地硬检查
    pub checks: StyleChecks,
}

impl StyleSpec {
    pub fn from_preset(p: &StylePreset) -> Self {
        Self {
            id: p.id.to_string(),
            name: p.name.to_string(),
            tagline: p.tagline.to_string(),
            word_target: p.word_target.to_string(),
            block: style_block(p),
            samples: p.samples.iter().map(|s| (*s).to_string()).collect(),
            rules: p.rules.iter().map(|s| (*s).to_string()).collect(),
            avoid: p.avoid.iter().map(|s| (*s).to_string()).collect(),
            bands: p.bands.to_vec(),
            checks: p.checks.clone(),
        }
    }

    /// 取本章的锚定样本（按章号轮换；无样本返回 None）
    pub fn sample_for(&self, idx: i64) -> Option<&str> {
        if self.samples.is_empty() {
            return None;
        }
        let i = (idx.rem_euclid(self.samples.len() as i64)) as usize;
        self.samples.get(i).map(|s| s.as_str())
    }

    /// 从用户自定义风格构建（S5）：卡片文本 + 指纹带 + 锚定样本
    pub fn from_custom(c: &crate::models::CustomStyle) -> Self {
        let card: serde_json::Value = serde_json::from_str(&c.card_json).unwrap_or_default();
        let fp: serde_json::Value = serde_json::from_str(&c.fingerprint_json).unwrap_or_default();
        let tagline = card["tagline"].as_str().unwrap_or("用户自定义风格").trim().to_string();
        let word_target = card["word_target"]
            .as_str()
            .unwrap_or("2000~2400 字")
            .trim()
            .to_string();
        let rules: Vec<String> = card["rules"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| sanitize_for_prompt(s.trim())))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let avoid: Vec<String> = card["avoid"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| sanitize_for_prompt(s.trim())))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let bands: Vec<FingerprintBand> = fp["bands"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|b| {
                        let metric = crate::style_fingerprint::static_metric_name(b["metric"].as_str()?)?;
                        let min = b["min"].as_f64()?;
                        let max = b["max"].as_f64()?;
                        if min < max {
                            Some(FingerprintBand { metric, min, max })
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut block = String::new();
        block.push_str(&format!("【自定义风格：{} —— {}】\n", c.name, tagline));
        block.push_str("平台节奏基线：\n");
        for b in PLATFORM_BASELINE {
            block.push_str(&format!("- {}\n", b.replace("{words}", &word_target)));
        }
        if !rules.is_empty() {
            block.push_str("风格规则：\n");
            for r in &rules {
                block.push_str(&format!("- {r}\n"));
            }
        }
        if !avoid.is_empty() {
            block.push_str("反例（出现即视为不合格）：\n");
            for a in &avoid {
                block.push_str(&format!("- {a}\n"));
            }
        }
        let anchor = sanitize_for_prompt(c.anchor.trim());
        let samples = if anchor.is_empty() { Vec::new() } else { vec![anchor] };
        Self {
            id: format!("custom:{}", c.id),
            name: c.name.clone(),
            tagline,
            word_target,
            block,
            samples,
            rules,
            avoid,
            bands,
            checks: StyleChecks {
                min_dialogue_ratio: None,
                max_sentence_len: None,
                forbid_modern_words: false,
            },
        }
    }
}

/// 注入提示词前的净化：避免污染 Mock 解析标记（真实提供方无此约束，但保持全局一致）
fn sanitize_for_prompt(s: &str) -> String {
    s.replace("TASK:", "TASK：")
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
        assert!(PRESETS.len() >= 20, "预设数量应完成 S2 扩容: {}", PRESETS.len());
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
            assert!(p.samples.len() >= 2, "{} 应至少有 2 段风格锚定样本", p.id);
            assert!(
                p.samples.iter().all(|s| s.chars().count() >= 60),
                "{} 的样本过短（<60 字）",
                p.id
            );
            assert!(p.bands.len() >= 5, "{} 的指纹带应至少 5 项", p.id);
            for b in p.bands {
                assert!(b.min < b.max, "{} 的指纹带区间非法: {}", p.id, b.metric);
            }
        }
    }

    #[test]
    fn samples_never_break_mock_markers() {
        // 样本会注入 user 提示词：同样不得包含 Mock 解析标记
        for p in PRESETS {
            for s in p.samples {
                for bad in ["=", "TASK:", "想法：", "原段落：", "chapter_index"] {
                    assert!(!s.contains(bad), "{} 的样本包含 Mock 标记 `{bad}`", p.id);
                }
            }
        }
    }

    #[test]
    fn style_spec_from_preset_carries_samples_and_bands() {
        let p = find("shadiao").unwrap();
        let spec = StyleSpec::from_preset(p);
        assert_eq!(spec.id, "shadiao");
        assert!(spec.block.contains("发疯毒舌"));
        assert!(spec.samples.len() >= 2);
        assert!(spec.bands.len() >= 5);
        assert_eq!(spec.sample_for(0), Some(spec.samples[0].as_str()));
        assert_eq!(spec.sample_for(2), spec.sample_for(0));
        assert_eq!(spec.sample_for(3), spec.sample_for(1));
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
