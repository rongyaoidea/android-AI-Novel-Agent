use anyhow::{anyhow, Context};
use futures_util::StreamExt;

use crate::models::{LlmSettings, TokenUsage, UsageRecord};

/// 一次 LLM 调用的返回：文本 + 归一化后的 token 用量
#[derive(Debug)]
pub struct LlmOutput {
    pub text: String,
    pub usage: TokenUsage,
}

/// 单次请求的最大尝试次数（含首次）
const MAX_ATTEMPTS: usize = 3;

/// 可自动重试的 HTTP 状态：限流 / 超时 / 服务端错误
pub fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

/// 退避时长：0.8s、1.6s…（约 -10% ~ +10% 抖动，避免多书同时重试形成尖峰）
pub fn backoff_delay(attempt: usize) -> std::time::Duration {
    let base_ms = 800u64.saturating_mul(1 << attempt.min(4));
    let span = base_ms / 5 + 1; // 最大抖动范围
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
        % span;
    std::time::Duration::from_millis(base_ms - base_ms / 10 + jitter)
}

/// 极简 LLM 客户端：
/// - provider = "mock"：离线模拟（用于测试 / 无 Key 演示）
/// - provider = "openai"：任意 OpenAI 兼容 /chat/completions（含流式 SSE 与用量解析）
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

    fn base_body(&self, system: &str, user: &str, stream: bool) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.settings.model,
            "temperature": self.settings.temperature,
            "stream": stream,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ]
        });
        if stream {
            // 让兼容实现（OpenAI / DeepSeek 等）在最后一个 chunk 里带回 usage
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }
        body
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.settings.api_key.is_empty() {
            req
        } else {
            req.bearer_auth(&self.settings.api_key)
        }
    }

    /// 非流式补全（含网络/限流/5xx 自动重试与退避）
    pub async fn complete(&self, system: &str, user: &str) -> anyhow::Result<LlmOutput> {
        if self.settings.provider == "mock" {
            let text = mock_complete(user)?;
            let usage = mock_usage(system, user, &text);
            return Ok(LlmOutput { text, usage });
        }
        let body = self.base_body(system, user, false);
        let resp = self
            .send_with_retry(|| self.auth(self.http.post(self.endpoint()).json(&body)))
            .await
            .context("LLM 请求失败（网络）")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("LLM HTTP {}: {}", status, truncate(&text, 300)));
        }
        let v: serde_json::Value = serde_json::from_str(&text).context("LLM 响应不是 JSON")?;
        let usage = parse_usage(&v);
        let out = v["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.trim().to_string())
            .ok_or_else(|| anyhow!("LLM 响应缺少 content 字段: {}", truncate(&text, 300)))?;
        Ok(LlmOutput { text: out, usage })
    }

    /// 带重试的请求发送：网络错误（超时/连接/请求）与 408/429/5xx 自动退避重试。
    /// 非可重试错误（400/401 等）原样返回，交由调用方处理。
    async fn send_with_retry(
        &self,
        make: impl Fn() -> reqwest::RequestBuilder,
    ) -> anyhow::Result<reqwest::Response> {
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..MAX_ATTEMPTS {
            match make().send().await {
                Ok(resp) => {
                    if is_retryable_status(resp.status()) && attempt + 1 < MAX_ATTEMPTS {
                        tracing::warn!(
                            "LLM HTTP {} 可重试，第 {}/{} 次退避后重试",
                            resp.status(),
                            attempt + 1,
                            MAX_ATTEMPTS - 1
                        );
                        tokio::time::sleep(backoff_delay(attempt)).await;
                        continue;
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    let retryable = e.is_timeout() || e.is_connect() || e.is_request();
                    if retryable && attempt + 1 < MAX_ATTEMPTS {
                        tracing::warn!(
                            "LLM 网络错误（{}），第 {}/{} 次退避后重试",
                            e,
                            attempt + 1,
                            MAX_ATTEMPTS - 1
                        );
                        tokio::time::sleep(backoff_delay(attempt)).await;
                        last_err = Some(e.into());
                        continue;
                    }
                    return Err(e.into());
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("LLM 请求失败（重试耗尽）")))
    }

    /// 流式生成：逐片回调 delta，返回完整文本与用量
    pub async fn stream(
        &self,
        system: &str,
        user: &str,
        mut on_delta: impl FnMut(&str) + Send,
    ) -> anyhow::Result<LlmOutput> {
        if self.settings.provider == "mock" {
            let text = mock_stream(user, &mut on_delta).await?;
            let usage = mock_usage(system, user, &text);
            return Ok(LlmOutput { text, usage });
        }

        // 部分兼容实现不认 stream_options：遇到 400 去掉该字段重试一次
        let mut body = self.base_body(system, user, true);
        let mut resp = self
            .send_with_retry(|| self.auth(self.http.post(self.endpoint()).json(&body)))
            .await
            .context("LLM 流式请求失败（网络）")?;
        if resp.status() == reqwest::StatusCode::BAD_REQUEST {
            body.as_object_mut().map(|m| m.remove("stream_options"));
            resp = self
                .auth(self.http.post(self.endpoint()).json(&body))
                .send()
                .await
                .context("LLM 流式重试失败（网络）")?;
        }
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("LLM HTTP {}: {}", status, truncate(&text, 300)));
        }

        let mut acc = String::new();
        let mut usage = TokenUsage::default();
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
                    // 末尾 chunk 通常 choices 为空、只带 usage
                    let u = parse_usage(&v);
                    if u != TokenUsage::default() {
                        usage = u;
                    }
                }
            }
        }
        if acc.trim().is_empty() {
            return Err(anyhow!("LLM 流式输出为空（模型未返回内容）"));
        }
        Ok(LlmOutput { text: acc, usage })
    }
}

/// 把不同厂商的 usage 结构归一化为 TokenUsage。
/// - OpenAI 系：`prompt_tokens` 已包含缓存命中部分（`prompt_tokens_details.cached_tokens`）；
///   DeepSeek 额外提供 `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`。
/// - Anthropic 系：`input_tokens` 不含缓存读写，需加上 `cache_read_*` / `cache_creation_*`。
/// 不变量：input = cache_hit + cache_miss（两者已知时）；total = input + output。
pub fn parse_usage(v: &serde_json::Value) -> TokenUsage {
    let u = &v["usage"];
    if !u.is_object() {
        return TokenUsage::default();
    }
    let n = |p: &str| u[p].as_i64();

    // ① OpenAI / DeepSeek 形态：prompt_tokens 为输入总量
    if let Some(input) = n("prompt_tokens") {
        let hit = u["prompt_tokens_details"]["cached_tokens"]
            .as_i64()
            .or_else(|| n("prompt_cache_hit_tokens"))
            .map(|h| h.clamp(0, input));
        let miss = n("prompt_cache_miss_tokens")
            .or_else(|| hit.map(|h| input - h))
            .map(|m| m.clamp(0, input));
        let output = n("completion_tokens");
        let total = n("total_tokens").or_else(|| output.map(|o| input + o));
        return TokenUsage {
            input: Some(input),
            output,
            cache_hit: hit,
            cache_miss: miss,
            total,
        };
    }

    // ② Anthropic 形态：input_tokens 是不含缓存的“新鲜”输入
    if let Some(fresh) = n("input_tokens") {
        let read = n("cache_read_input_tokens").unwrap_or(0);
        let creation = n("cache_creation_input_tokens").unwrap_or(0);
        let input = fresh + read + creation;
        let output = n("output_tokens");
        let total = n("total_tokens").or_else(|| output.map(|o| input + o));
        return TokenUsage {
            input: Some(input),
            output,
            cache_hit: n("cache_read_input_tokens"),
            cache_miss: Some(fresh + creation),
            total,
        };
    }

    // ③ 只有总量
    TokenUsage {
        total: n("total_tokens"),
        ..TokenUsage::default()
    }
}

/// 记录一次调用的用量（失败仅记录日志，不阻断创作）
pub fn record_usage(
    db: &crate::db::Db,
    novel_id: Option<&str>,
    chapter_idx: Option<i64>,
    task: &str,
    settings: &LlmSettings,
    usage: &TokenUsage,
) {
    let rec = UsageRecord {
        id: uuid::Uuid::new_v4().to_string(),
        novel_id: novel_id.map(|s| s.to_string()),
        chapter_idx,
        task: task.to_string(),
        provider: settings.provider.clone(),
        model: settings.model.clone(),
        input: usage.input,
        output: usage.output,
        cache_hit: usage.cache_hit,
        cache_miss: usage.cache_miss,
        total: usage.total,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(e) = db.insert_usage(&rec) {
        tracing::warn!("record usage failed: {e:#}");
    }
}

// ---------------- mock provider ----------------

/// mock 模式下给出确定性的用量，便于端到端校验统计口径
fn mock_usage(system: &str, user: &str, text: &str) -> TokenUsage {
    let input = ((system.chars().count() + user.chars().count()) / 2) as i64;
    let output = (text.chars().count() / 2) as i64;
    let hit = input / 2;
    TokenUsage {
        input: Some(input),
        output: Some(output),
        cache_hit: Some(hit),
        cache_miss: Some(input - hit),
        total: Some(input + output),
    }
}

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
    use serde_json::json;

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

    // ---------- 用量解析（核心口径） ----------

    #[test]
    fn usage_missing_is_unknown_not_zero() {
        assert_eq!(parse_usage(&json!({})), TokenUsage::default());
        assert_eq!(parse_usage(&json!({"usage": null})), TokenUsage::default());
        let u = parse_usage(&json!({"usage": {}}));
        assert_eq!(u.input, None, "缺字段必须是未知，而不是 0");
        assert_eq!(u.total, None);
    }

    #[test]
    fn usage_openai_basic() {
        let u = parse_usage(&json!({"usage": {"prompt_tokens": 100, "completion_tokens": 50}}));
        assert_eq!(u.input, Some(100));
        assert_eq!(u.output, Some(50));
        assert_eq!(u.cache_hit, None, "未提供缓存字段 → 未知");
        assert_eq!(u.total, Some(150), "total = input + output");
    }

    #[test]
    fn usage_openai_cached_details() {
        let u = parse_usage(&json!({"usage": {
            "prompt_tokens": 1000, "completion_tokens": 100, "total_tokens": 1100,
            "prompt_tokens_details": {"cached_tokens": 800}
        }}));
        assert_eq!(u.input, Some(1000));
        assert_eq!(u.cache_hit, Some(800));
        assert_eq!(u.cache_miss, Some(200), "未命中 = 输入总量 - 命中");
        assert_eq!(u.total, Some(1100));
        assert_eq!(u.total, Some(u.input.unwrap() + u.output.unwrap()));
        assert_eq!(u.input, Some(u.cache_hit.unwrap() + u.cache_miss.unwrap()));
    }

    #[test]
    fn usage_deepseek_hit_miss_pair() {
        let u = parse_usage(&json!({"usage": {
            "prompt_tokens": 1000, "completion_tokens": 80,
            "prompt_cache_hit_tokens": 900, "prompt_cache_miss_tokens": 100
        }}));
        assert_eq!(u.cache_hit, Some(900));
        assert_eq!(u.cache_miss, Some(100));
        assert_eq!(u.total, Some(1080));
    }

    #[test]
    fn usage_anthropic_normalizes_cache() {
        // Anthropic：input_tokens 不含缓存读写
        let u = parse_usage(&json!({"usage": {
            "input_tokens": 80, "output_tokens": 30,
            "cache_read_input_tokens": 900, "cache_creation_input_tokens": 20
        }}));
        assert_eq!(u.input, Some(1000), "输入总量 = 新鲜 + 读缓存 + 建缓存");
        assert_eq!(u.cache_hit, Some(900));
        assert_eq!(u.cache_miss, Some(100), "未命中含新写入缓存");
        assert_eq!(u.total, Some(1030));
    }

    #[test]
    fn usage_clamps_inconsistent_cached() {
        let u = parse_usage(&json!({"usage": {
            "prompt_tokens": 500, "completion_tokens": 10,
            "prompt_tokens_details": {"cached_tokens": 800}
        }}));
        assert_eq!(u.cache_hit, Some(500), "命中数不得超过输入总量");
        assert_eq!(u.cache_miss, Some(0));
    }

    #[test]
    fn usage_only_total() {
        let u = parse_usage(&json!({"usage": {"total_tokens": 42}}));
        assert_eq!(u.total, Some(42));
        assert_eq!(u.input, None);
        assert_eq!(u.output, None);
    }

    #[test]
    fn usage_stream_final_chunk() {
        // 流式结尾 chunk：choices 为空、只带 usage
        let chunk = json!({"choices": [], "usage": {"prompt_tokens": 300, "completion_tokens": 200,
            "prompt_tokens_details": {"cached_tokens": 100}}});
        let u = parse_usage(&chunk);
        assert_eq!(u.input, Some(300));
        assert_eq!(u.output, Some(200));
        assert_eq!(u.cache_hit, Some(100));
        assert_eq!(u.cache_miss, Some(200));
        assert_eq!(u.total, Some(500));
    }

    #[test]
    fn mock_usage_is_consistent() {
        let u = mock_usage("sys", "user prompt", "generated text");
        assert_eq!(u.total, Some(u.input.unwrap() + u.output.unwrap()));
        assert_eq!(u.input, Some(u.cache_hit.unwrap() + u.cache_miss.unwrap()));
        assert!(u.cache_hit.unwrap() <= u.input.unwrap());
    }

    // ---------- 重试与退避 ----------

    #[test]
    fn retryable_status_classification() {
        use reqwest::StatusCode;
        assert!(is_retryable_status(StatusCode::REQUEST_TIMEOUT));
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
        assert!(!is_retryable_status(StatusCode::NOT_FOUND));
    }

    #[test]
    fn backoff_is_increasing_and_bounded() {
        let d0 = backoff_delay(0).as_millis() as u64;
        let d1 = backoff_delay(1).as_millis() as u64;
        let d2 = backoff_delay(2).as_millis() as u64;
        assert!((600..=1000).contains(&d0), "d0={d0}");
        assert!(d1 > d0, "d1={d1} d0={d0}");
        assert!(d2 > d1, "d2={d2} d1={d1}");
        assert!(backoff_delay(10).as_millis() <= 20_000, "上限应受约束");
    }

    fn flaky_app(hits: std::sync::Arc<std::sync::atomic::AtomicUsize>, fail_times: usize) -> axum::Router {
        use axum::response::IntoResponse;
        axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move || {
                let hits = hits.clone();
                async move {
                    let n = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n < fail_times {
                        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response();
                    }
                    axum::Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok-text"}}],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
                    }))
                    .into_response()
                }
            }),
        )
    }

    fn llm_for(addr: std::net::SocketAddr) -> Llm {
        Llm::new(LlmSettings {
            provider: "openai".into(),
            base_url: format!("http://{addr}/v1"),
            api_key: String::new(),
            model: "test-model".into(),
            temperature: 0.5,
        })
    }

    /// 集成：前两次 500、第三次成功 → 自动重试后正常返回（含用量解析）
    #[tokio::test]
    async fn retries_on_5xx_then_succeeds() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let app = flaky_app(hits.clone(), 2);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let llm = llm_for(addr);
        let out = llm.complete("system", "user").await.unwrap();
        assert_eq!(out.text, "ok-text");
        assert_eq!(hits.load(Ordering::SeqCst), 3, "应为 2 次失败 + 1 次成功");
        assert_eq!(out.usage.input, Some(10));
    }

    /// 集成：持续 500 → 重试耗尽返回错误，且尝试次数受限（不会无限重试）
    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let app = flaky_app(hits.clone(), usize::MAX);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let llm = llm_for(addr);
        let err = llm.complete("system", "user").await.unwrap_err();
        assert!(format!("{err}").contains("500"), "错误应带状态码: {err}");
        assert_eq!(hits.load(Ordering::SeqCst), 3, "最多尝试 3 次");
    }
}
