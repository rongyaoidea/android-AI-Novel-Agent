use crate::models::{Chapter, Novel};

/// 导出为纯文本（后续可扩展 EPUB）。
pub fn novel_to_txt(novel: &Novel, chapters: &[Chapter]) -> String {
    let mut out = String::new();
    let title = if novel.title.trim().is_empty() {
        "未命名"
    } else {
        novel.title.trim()
    };
    out.push_str(title);
    out.push('\n');
    if !novel.logline.trim().is_empty() {
        out.push_str(novel.logline.trim());
        out.push('\n');
    }
    out.push('\n');
    for ch in chapters.iter().filter(|c| !c.content.trim().is_empty()) {
        out.push_str(&format!("第{}章 {}\n\n", ch.idx, ch.title.trim()));
        out.push_str(ch.content.trim());
        out.push_str("\n\n");
    }
    out.trim_end().to_string() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_readable_text() {
        let novel = Novel {
            id: "n1".into(),
            title: "深海测试".into(),
            genre: "奇幻".into(),
            idea: "灵感".into(),
            logline: "一句话卖点".into(),
            protagonist: "主角".into(),
            style: "基调".into(),
            book_outline: "走向".into(),
            status: "drafted".into(),
            mode: "auto".into(),
            style_preset: String::new(),
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        let ch = Chapter {
            id: "c1".into(),
            novel_id: "n1".into(),
            idx: 1,
            title: "起点".into(),
            goal: "".into(),
            beats: vec![],
            content: "正文内容。".into(),
            summary: String::new(),
            status: "done".into(),
            review_json: None,
            style_report: String::new(),
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        let text = novel_to_txt(&novel, &[ch]);
        assert!(text.contains("深海测试"));
        assert!(text.contains("第1章 起点"));
        assert!(text.contains("正文内容。"));
    }
}
