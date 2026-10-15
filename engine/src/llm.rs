use anyhow::{anyhow, Context};
use futures_util::StreamExt;

use crate::models::LlmSettings;

/// 极简 LLM 客户端：
/// - provider = "mock"：离线模拟（用于测试 / 无 Key 演示）
/// - provider = "openai"：任意 OpenAI 兼容 /chat/completions（含流式 SSE）
pub struct Llm {
    settings: LlmSettings,
    http: reqwest::Client,
}

impl Llm {
    pub fn new(settings: LlmSettings) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .connect_timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Self { settings, http }
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.settings.base_url.trim_end_matches('/'))
    }

    pub async fn complete(&self, system: &str, user: &str) -> anyhow::Result<String> {
        if self.settings.provider == "mock" {
            return mock_complete(user);
        }
        let body = serde_json::json!({
            "model": self.settings.model,
            "temperature": self.settings.temperature,
            "stream": false,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ]
        });
        let mut req = self.http.post(self.endpoint()).json(&body);
        if !self.settings.api_key.is_empty() {
            req = req.bearer_auth(&self.settings.api_key);
        }
        let resp = req.send().await.context("LLM 请求失败（网络）")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("LLM HTTP {}: {}", status, truncate(&text, 300)));
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).context("LLM 响应不是 JSON")?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.trim().to_string())
            .ok_or_else(|| anyhow!("LLM 响应缺少 content 字段: {}", truncate(&text, 300)))
    }

    /// 流式生成：逐片回调 delta，返回完整文本
    pub async fn stream(
        &self,
        system: &str,
        user: &str,
        mut on_delta: impl FnMut(&str) + Send,
    ) -> anyhow::Result<String> {
        if self.settings.provider == "mock" {
            return mock_stream(user, &mut on_delta).await;
        }
        let body = serde_json::json!({
            "model": self.settings.model,
            "temperature": self.settings.temperature,
            "stream": true,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ]
        });
        let mut req = self.http.post(self.endpoint()).json(&body);
        if !self.settings.api_key.is_empty() {
            req = req.bearer_auth(&self.settings.api_key);
        }
        let resp = req.send().await.context("LLM 流式请求失败（网络）")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("LLM HTTP {}: {}", status, truncate(&text, 300)));
        }
        let mut acc = String::new();
        let mut buf = String::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("LLM 流读取失败")?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim_end_matches('\r').to_string();
                buf.drain(..=pos);
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(delta) = v["choices"][0]["delta"]["content"].as_str() {
                        if !delta.is_empty() {
                            acc.push_str(delta);
                            on_delta(delta);
                        }
                    }
                }
            }
        }
        if acc.trim().is_empty() {
            return Err(anyhow!("LLM 流式输出为空（模型未返回内容）"));
        }
        Ok(acc)
    }
}

// ---------------- mock provider ----------------

fn mock_complete(user: &str) -> anyhow::Result<String> {
    if user.contains("TASK:brief") {
        let idea = extract_after(user, "想法：").unwrap_or_else(|| "未命名灵感".into());
        let short: String = idea.chars().take(12).collect();
        return Ok(serde_json::json!({
            "title": format!("{}·试作", short),
            "genre": "奇幻",
            "logline": format!("围绕「{}」展开的一段命运之旅。", short),
            "protagonist": "林深 —— 一个被卷入浪潮的普通人",
            "style": "节奏明快，第三人称限知视角，网文笔法"
        })
        .to_string());
    }
    if user.contains("TASK:outline") {
        let from = extract_number_after(user, "from=").unwrap_or(1).clamp(1, 50);
        let to = extract_number_after(user, "to=").unwrap_or(from + 2).clamp(from, 50);
        let mut chapters = Vec::new();
        for i in from..=to {
            chapters.push(serde_json::json!({
                "idx": i,
                "title": format!("风起之{}", i),
                "goal": format!("推进第 {} 阶段目标", i),
                "beats": ["开场钩子", "冲突升级", "章末悬念"]
            }));
        }
        return Ok(serde_json::json!({
            "book_outline": format!("全书规划 {} 章：主角从平凡日常被卷入，逐步走向终局决战。", to),
            "chapters": chapters
        })
        .to_string());
    }
    if user.contains("TASK:chapter") {
        let idx = extract_number_after(user, "index=").unwrap_or(1);
        let title = extract_after(user, "title=").unwrap_or_else(|| format!("第{}章", idx));
        let para = format!(
            "（模拟模式）这是第{}章《{}》的正文示例段落，用来在离线环境验证生成流水线、流式传输与保存链路。",
            idx, title
        );
        let mut text = format!("第{}章 {}\n\n", idx, title);
        for _ in 0..6 {
            text.push_str(&para);
            text.push_str(" 风从窗缝里钻进来，桌上的纸页轻轻翻了一下，像在提醒他时间不多。\n\n");
        }
        return Ok(text);
    }
    if user.contains("TASK:review") {
        return Ok(serde_json::json!({
            "ok": true,
            "issues": [{"severity": "info", "note": "模拟审校：未发现硬性缺口。", "quote": ""}]
        })
        .to_string());
    }
    if user.contains("TASK:polish") {
        let selection = extract_between(user, "原段落：\n---\n", "\n---").unwrap_or_default();
        let mode = extract_after(user, "mode=").unwrap_or_else(|| "rewrite".into());
        return Ok(match mode.as_str() {
            "expand" => format!("{selection} 她停了一下，又补了一句：「事情没那么简单。」"),
            "shrink" => {
                let keep = selection.chars().count() * 6 / 10;
                let cut: String = selection.chars().take(keep.max(1)).collect();
                format!("{cut}……")
            }
            "depolish" => selection.replace("不禁", "").replace("仿佛", "像"),
            _ => format!("（改写）{selection}"),
        });
    }
    if user.contains("TASK:extract") {
        let idx = extract_number_after(user, "chapter_index=").unwrap_or(1);
        let mut planted = vec![serde_json::json!({ "title": format!("第{idx}章留下的悬念") })];
        let mut resolved: Vec<serde_json::Value> = Vec::new();
        if idx > 1 {
            resolved.push(serde_json::json!({ "title": format!("第{}章留下的悬念", idx - 1) }));
        } else {
            planted = vec![serde_json::json!({ "title": "第1章留下的悬念" })];
        }
        return Ok(serde_json::json!({
            "summary": format!("第{idx}章：林深继续寻找姐姐，线索推进，章末留悬念。"),
            "characters": [
                { "name": "林深", "role": "主角", "status": "active",
                  "facts": [format!("第{idx}章：继续寻找姐姐")] }
            ],
            "foreshadowings_planted": planted,
            "foreshadowings_resolved": resolved
        })
        .to_string());
    }
    if user.contains("TASK:continue") {
        let chars = extract_number_after(user, "chars=").unwrap_or(600).clamp(100, 2000);
        let unit = "（续写）雨声更密了些，他把书合上，向巷口走去。路灯把影子拉得很长。";
        let mut out = String::new();
        while (out.chars().count() as i64) < chars.min(600) {
            out.push_str(unit);
        }
        return Ok(out);
    }
    if user.contains("TASK:rewrite") {
        let mode = extract_token_after(user, "mode=").unwrap_or_else(|| "revise".into());
        let title = extract_after(user, "title=").unwrap_or_else(|| "第X章".into());
        if mode == "regenerate" {
            return Ok(format!(
                "第X章 {title}\n\n（重新生成）风从长街尽头卷来，带着雨后的潮气。\n\n他站在门口，没有立刻进去——有些决定，一旦迈出就没有回头路。"
            ));
        }
        return Ok(format!(
            "第X章 {title}\n\n（修订版）雨停的时候，他终于推开了那扇门。\n\n屋里的人抬起头，谁也没有先开口。"
        ));
    }
    Ok("（模拟模式）未识别的任务。".to_string())
}

async fn mock_stream(user: &str, on_delta: &mut impl FnMut(&str)) -> anyhow::Result<String> {
    let full = mock_complete(user)?;
    let mut acc = String::new();
    for piece in full.split_inclusive('\n') {
        tokio::time::sleep(std::time::Duration::from_millis(12)).await;
        acc.push_str(piece);
        on_delta(piece);
    }
    Ok(acc)
}

// ---------------- 工具函数 ----------------

fn extract_after(text: &str, marker: &str) -> Option<String> {
    let pos = text.find(marker)? + marker.len();
    let rest = &text[pos..];
    let end = rest.find('\n').unwrap_or(rest.len());
    let v = rest[..end].trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

fn extract_number_after(text: &str, marker: &str) -> Option<i64> {
    let pos = text.find(marker)? + marker.len();
    let digits: String = text[pos..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn extract_between(text: &str, start: &str, end: &str) -> Option<String> {
    let s = text.find(start)? + start.len();
    let rest = &text[s..];
    let e = rest.find(end)?;
    Some(rest[..e].trim().to_string())
}

/// 取 marker 之后的连续字母/数字 token（不受同行后续内容影响）
fn extract_token_after(text: &str, marker: &str) -> Option<String> {
    let pos = text.find(marker)? + marker.len();
    let token: String = text[pos..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// 从可能带 markdown 围栏 / 前后缀的 LLM 文本里抽出第一个 JSON 对象。
pub fn extract_json_object(text: &str) -> Option<serde_json::Value> {
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    let mut end = None;
    for (i, ch) in text.char_indices() {
        if i < start {
            continue;
        }
        if in_str {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    serde_json::from_str(&text[start..end]).ok()
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_json_from_fenced_text() {
        let text = "好的，结果如下：\n```json\n{\"a\": 1, \"b\": {\"c\": \"}}\"}}\n```\n以上。";
        let v = extract_json_object(text).expect("should parse");
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"]["c"], "}}");
    }

    #[test]
    fn mock_brief_has_marker() {
        let out = mock_complete("TASK:brief\n想法：深海与鲸歌\n").unwrap();
        assert!(out.contains("title"));
    }
}
