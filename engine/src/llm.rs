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
    /// 候选端点（按优先级；Base URL 缺 /v1 时提供兜底，修复常见 404）
    endpoints: Vec<String>,
    /// OpenCode（Zen/Go）要求的稳定会话标识（其它服务商为 None）
    session: Option<String>,
}

/// 归一化已知的过时 / 易错 Base URL（在解析端点前调用）
/// - 早前版本预设的 OpenCode Go 地址（`/zen/go/v1`）→ 官方文档端点
pub fn normalize_base_url(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    let lower = b.to_ascii_lowercase();
    if lower == "https://opencode.ai/zen/go" || lower == "https://opencode.ai/zen/go/v1" {
        return "https://opencode.ai/inference/go/openai/v1".to_string();
    }
    b.to_string()
}

/// OpenCode（Zen / Go）要求客户端提供稳定会话标识（x-opencode-session）
fn needs_opencode_session(base_url: &str) -> bool {
    base_url.to_ascii_lowercase().contains("opencode.ai")
}

/// 解析可用的 chat/completions 端点候选：
/// - Base URL 已含 /chat/completions → 原样使用
/// - 否则先试 `{base}/chat/completions`；若 Base 末段不是版本段（v1/v2/…），
///   再加入 `{base}/v1/chat/completions` 兜底
///   （用户常只填域名，如 `https://api.openai.com`，缺 `/v1` 会 404）
pub fn endpoint_candidates(base_url: &str) -> Vec<String> {
    let b = normalize_base_url(base_url);
    let b = b.trim().trim_end_matches('/');
    if b.is_empty() {
        return vec!["/chat/completions".to_string()];
    }
    if b.ends_with("/chat/completions") {
        return vec![b.to_string()];
    }
    let mut out = vec![format!("{b}/chat/completions")];
    let last_seg = b.rsplit('/').next().unwrap_or("");
    let is_version_seg = last_seg.len() >= 2
        && last_seg.starts_with('v')
        && last_seg[1..].chars().all(|c| c.is_ascii_digit());
    if !is_version_seg {
        out.push(format!("{b}/v1/chat/completions"));
    }
    out
}

/// 解析可用的 /models 端点候选（与 chat 端点同构；用于「拉取模型列表」）
pub fn model_list_candidates(base_url: &str) -> Vec<String> {
    let b0 = normalize_base_url(base_url);
    let b = b0.trim_end_matches('/');
    // OpenCode Go：模型列表在 `inference/go/v1/models`（与 chat 的 /openai/v1 不同前缀）
    let lower = b.to_ascii_lowercase();
    if lower.ends_with("/inference/go/openai/v1") {
        let prefix = &b[..b.len() - "/openai/v1".len()];
        return vec![format!("{prefix}/v1/models"), format!("{b}/models")];
    }
    let b = b
        .strip_suffix("/chat/completions")
        .unwrap_or(b)
        .trim_end_matches('/');
    if b.is_empty() {
        return vec!["/models".to_string()];
    }
    let mut out = vec![format!("{b}/models")];
    let last_seg = b.rsplit('/').next().unwrap_or("");
    let is_version_seg = last_seg.len() >= 2
        && last_seg.starts_with('v')
        && last_seg[1..].chars().all(|c| c.is_ascii_digit());
    if !is_version_seg {
        out.push(format!("{b}/v1/models"));
    }
    out
}

impl Llm {
    /// 拉取服务商模型列表（GET {base}/models，OpenAI 兼容格式）。
    /// Base URL 缺 /v1 时自动回退；错误信息包含已尝试端点。
    pub async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        let urls = model_list_candidates(&self.settings.base_url);
        let mut last: Option<(reqwest::StatusCode, String)> = None;
        for (i, url) in urls.iter().enumerate() {
            let resp = self
                .send_with_retry(|| self.decorate(self.http.get(url)))
                .await
                .context("拉取模型列表失败（网络）")?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                let v: serde_json::Value =
                    serde_json::from_str(&text).context("模型列表响应不是 JSON")?;
                let mut ids: Vec<String> = v["data"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|m| m["id"].as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                if ids.is_empty() {
                    anyhow::bail!(
                        "未解析到模型（响应非 OpenAI /models 格式）: {}",
                        truncate(&text, 200)
                    );
                }
                ids.sort();
                ids.dedup();
                return Ok(ids);
            }
            if status == reqwest::StatusCode::NOT_FOUND {
                if i + 1 < urls.len() {
                    tracing::warn!(
                        "模型列表端点 404（{url}），尝试备用端点 {url2}",
                        url = url,
                        url2 = &urls[i + 1]
                    );
                    last = Some((status, text));
                    continue;
                }
                anyhow::bail!("模型列表接口不存在（已尝试端点：{}）", urls.join("、"));
            }
            anyhow::bail!(
                "拉取模型列表 HTTP {} @ {}: {}",
                status,
                url,
                truncate(&text, 300)
            );
        }
        let (status, text) = last.unwrap_or((reqwest::StatusCode::NOT_FOUND, String::new()));
        anyhow::bail!(
            "拉取模型列表 HTTP {}（已尝试端点：{}）: {}",
            status,
            urls.join("、"),
            truncate(&text, 300)
        )
    }
}

impl Llm {
    pub fn new(settings: LlmSettings) -> Self {
        // 归一化 Base URL（含早前预设的 OpenCode Go 地址纠正）
        let mut settings = settings;
        settings.base_url = normalize_base_url(&settings.base_url);

        let http = reqwest::Client::builder()
            // 标识本应用（OpenCode Go 等要求非通用 SDK 的 UA）
            .user_agent(format!(
                "android-AI-Novel-Agent/{}",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(std::time::Duration::from_secs(600))
            .connect_timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        let endpoints = endpoint_candidates(&settings.base_url);
        // OpenCode（Zen/Go）要求稳定会话标识用于路由与提示缓存
        let session = needs_opencode_session(&settings.base_url)
            .then(|| uuid::Uuid::new_v4().to_string());
        Self {
            settings,
            http,
            endpoints,
            session,
        }
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

    /// 鉴权 + 平台附加头（OpenCode：x-opencode-session）
    fn decorate(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let req = self.auth(req);
        match &self.session {
            Some(s) => req.header("x-opencode-session", s),
            None => req,
        }
    }

    /// 非流式补全（含网络/限流/5xx 自动重试与退避；Base URL 缺 /v1 时自动回退尝试）
    pub async fn complete(&self, system: &str, user: &str) -> anyhow::Result<LlmOutput> {
        if self.settings.provider == "mock" {
            let text = mock_complete(user)?;
            let usage = mock_usage(system, user, &text);
            return Ok(LlmOutput { text, usage });
        }
        let body = self.base_body(system, user, false);
        let mut last: Option<(reqwest::StatusCode, String)> = None;
        for (i, url) in self.endpoints.iter().enumerate() {
            let resp = self
                .send_with_retry(|| self.decorate(self.http.post(url).json(&body)))
                .await
                .context("LLM 请求失败（网络）")?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                let v: serde_json::Value =
                    serde_json::from_str(&text).context("LLM 响应不是 JSON")?;
                let usage = parse_usage(&v);
                let out = v["choices"][0]["message"]["content"]
                    .as_str()
                    .map(|s| s.trim().to_string())
                    .ok_or_else(|| {
                        anyhow!("LLM 响应缺少 content 字段: {}", truncate(&text, 300))
                    })?;
                return Ok(LlmOutput { text: out, usage });
            }
            // 404 且还有候选端点 → 换下一个（常见于 Base URL 少写 /v1）
            if status == reqwest::StatusCode::NOT_FOUND {
                if i + 1 < self.endpoints.len() {
                    tracing::warn!(
                        "LLM 端点 404（{url}），尝试备用端点 {url2}",
                        url = url,
                        url2 = &self.endpoints[i + 1]
                    );
                    last = Some((status, text));
                    continue;
                }
                return Err(anyhow!(
                    "LLM HTTP {}（已尝试端点：{}）: {}",
                    status,
                    self.endpoints.join("、"),
                    truncate(&text, 300)
                ));
            }
            return Err(anyhow!(
                "LLM HTTP {} @ {}: {}",
                status,
                url,
                truncate(&text, 300)
            ));
        }
        let (status, text) = last.unwrap_or((reqwest::StatusCode::NOT_FOUND, String::new()));
        Err(anyhow!(
            "LLM HTTP {}（已尝试端点：{}）: {}",
            status,
            self.endpoints.join("、"),
            truncate(&text, 300)
        ))
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

        // 部分兼容实现不认 stream_options：遇到 400 去掉该字段重试一次；404 时回退备用端点
        let mut body = self.base_body(system, user, true);
        let mut resp: Option<reqwest::Response> = None;
        let mut last: Option<(reqwest::StatusCode, String)> = None;
        for (i, url) in self.endpoints.iter().enumerate() {
            let r = self
                .send_with_retry(|| self.decorate(self.http.post(url).json(&body)))
                .await
                .context("LLM 流式请求失败（网络）")?;
            if r.status() == reqwest::StatusCode::BAD_REQUEST {
                body.as_object_mut().map(|m| m.remove("stream_options"));
                let r2 = self
                    .send_with_retry(|| self.decorate(self.http.post(url).json(&body)))
                    .await
                    .context("LLM 流式重试失败（网络）")?;
                if r2.status().is_success() {
                    resp = Some(r2);
                    break;
                }
                let st = r2.status();
                let text = r2.text().await.unwrap_or_default();
                if st == reqwest::StatusCode::NOT_FOUND && i + 1 < self.endpoints.len() {
                    last = Some((st, text));
                    continue;
                }
                return Err(anyhow!("LLM HTTP {} @ {}: {}", st, url, truncate(&text, 300)));
            }
            if r.status().is_success() {
                resp = Some(r);
                break;
            }
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::NOT_FOUND {
                if i + 1 < self.endpoints.len() {
                    tracing::warn!(
                        "LLM 流式端点 404（{url}），尝试备用端点 {url2}",
                        url = url,
                        url2 = &self.endpoints[i + 1]
                    );
                    last = Some((status, text));
                    continue;
                }
                return Err(anyhow!(
                    "LLM HTTP {}（已尝试端点：{}）: {}",
                    status,
                    self.endpoints.join("、"),
                    truncate(&text, 300)
                ));
            }
            return Err(anyhow!(
                "LLM HTTP {} @ {}: {}",
                status,
                url,
                truncate(&text, 300)
            ));
        }
        let Some(resp) = resp else {
            let (status, text) = last.unwrap_or((reqwest::StatusCode::NOT_FOUND, String::new()));
            return Err(anyhow!(
                "LLM HTTP {}（已尝试端点：{}）: {}",
                status,
                self.endpoints.join("、"),
                truncate(&text, 300)
            ));
        };

        let mut acc = String::new();
        let mut usage = TokenUsage::default();
        let mut buf = String::new();
        let mut pending: Vec<u8> = Vec::new();
        let mut stream = resp.bytes_stream();
        // 单行解析：data: 行 → delta + usage
        fn parse_line(
            line: &str,
            acc: &mut String,
            usage: &mut TokenUsage,
            on_delta: &mut dyn FnMut(&str),
        ) {
            let Some(data) = line.trim_end_matches('\r').strip_prefix("data:") else {
                return;
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                return;
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
                    *usage = u;
                }
            }
        }
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("LLM 流读取失败")?;
            pending.extend_from_slice(&chunk);
            // 只解码「完整的 UTF-8 前缀」：多字节字符跨 chunk 时留到下一块，避免写入 �
            let mut text = String::new();
            loop {
                match std::str::from_utf8(&pending) {
                    Ok(s) => {
                        text.push_str(s);
                        pending.clear();
                        break;
                    }
                    Err(e) => {
                        let valid = e.valid_up_to();
                        if valid > 0 {
                            text.push_str(std::str::from_utf8(&pending[..valid]).unwrap_or(""));
                            pending.drain(..valid);
                        }
                        if e.error_len().is_some() {
                            // 明确的非法字节：跳过一个字节，避免死循环
                            if !pending.is_empty() {
                                pending.remove(0);
                            }
                        } else {
                            break; // 尾部不完整，等下一块
                        }
                    }
                }
            }
            buf.push_str(&text);
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].to_string();
                buf.drain(..=pos);
                parse_line(&line, &mut acc, &mut usage, &mut on_delta);
            }
        }
        // 末尾可能没有换行的最后一帧（常见于只带 usage 的收尾帧）
        if !buf.trim().is_empty() {
            let line = buf.clone();
            parse_line(&line, &mut acc, &mut usage, &mut on_delta);
        }
        if acc.trim().is_empty() {
            return Err(anyhow::Error::new(EmptyStreamError));
        }
        Ok(LlmOutput { text: acc, usage })
    }
}

/// 流式输出为空（模型 200 但未返回任何内容）。
/// 单独的类型便于调用方识别并重试，而不是把整批运行判失败。
#[derive(Debug)]
pub struct EmptyStreamError;

impl std::fmt::Display for EmptyStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LLM 流式输出为空（模型未返回内容）")
    }
}

impl std::error::Error for EmptyStreamError {}

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
            "worldview": "近代沿海孤城，风讯台维系航海秩序；识潮人可听见深海讯号。",
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
    if user.contains("TASK:foreshadow_audit") {
        // 离线演示不臆造回收结论
        return Ok(serde_json::json!({"resolved": []}).to_string());
    }
    if user.contains("TASK:volume_summary") {
        let to = extract_number_after(user, "to=").unwrap_or(0);
        return Ok(serde_json::json!({
            "summary": format!("（模拟）本卷行至第 {to} 章：主角逐步逼近真相，关键伏笔多条埋设，仍有悬念待解。")
        })
        .to_string());
    }
    if user.contains("TASK:global_review") {
        // 离线演示不臆造跨章问题（保持审校系统可预测）
        return Ok(serde_json::json!({"ok": true, "issues": []}).to_string());
    }
    if user.contains("TASK:review") {
        return Ok(serde_json::json!({
            "ok": true,
            "issues": [{
                "severity": "medium",
                "note": "模拟审校：建议微调一处段落节奏，避免匀速推进。",
                "quote": ""
            }]
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

    /// 集成：流式解析的字节边界与收尾帧
    /// ① 多字节字符跨 chunk 不产生 �；② 末行无换行的 usage 帧不丢；③ 空流返回可识别的 EmptyStreamError
    #[tokio::test]
    async fn stream_handles_chunk_splits_and_empty_output() {
        use futures_util::stream;
        // 「他」= E4 BB 96：让第一个 chunk 停在第一个字节之后
        let head = "data: {\"choices\":[{\"delta\":{\"content\":\"".to_string();
        let tail = "他\"}}]}\n\n".to_string();
        let bytes = tail.as_bytes();
        let pos = bytes.iter().position(|b| *b == 0xE4).expect("包含汉字");
        let part1 = [head.as_bytes(), &bytes[..pos + 1]].concat();
        let part2 = bytes[pos + 1..].to_vec();
        // 收尾 usage 帧：故意不带结尾换行
        let usage_json = serde_json::json!({
            "choices": [],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3}
        })
        .to_string();
        let usage_frame = format!("data: {usage_json}");

        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move || {
                let p1 = part1.clone();
                let p2 = part2.clone();
                let uf = usage_frame.clone();
                async move {
                    let s = stream::iter(vec![
                        Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(p1)),
                        Ok(axum::body::Bytes::from(p2)),
                        Ok(axum::body::Bytes::from(uf.into_bytes())),
                    ]);
                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from_stream(s))
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let llm = llm_for(addr);
        fn noop(_: &str) {}
        let out = llm.stream("s", "u", noop).await.unwrap();
        assert!(out.text.contains('他'), "跨 chunk 的多字节字符应完整：{:?}", out.text);
        assert!(!out.text.contains('\u{FFFD}'), "不应出现替换字符：{:?}", out.text);
        assert_eq!(out.usage.input, Some(7), "无换行的收尾 usage 帧也要解析");
        assert_eq!(out.usage.output, Some(3));
        assert_eq!(out.usage.total, Some(10));

        // 空流（只有 [DONE]）→ EmptyStreamError（供上层的「空内容自动重试一次」识别）
        let empty_app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|| async {
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from("data: [DONE]\n\n"))
                    .unwrap()
            }),
        );
        let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr2 = listener2.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener2, empty_app).await;
        });
        let llm2 = llm_for(addr2);
        let err = llm2.stream("s", "u", noop).await.unwrap_err();
        assert!(
            err.downcast_ref::<EmptyStreamError>().is_some(),
            "应为可识别的空流错误：{err}"
        );
    }

    // ---------- 端点解析与 404 回退 ----------

    #[test]
    fn endpoint_candidates_variants() {
        let c = |b: &str| endpoint_candidates(b);
        assert_eq!(
            c("https://api.x.com/v1/chat/completions"),
            vec!["https://api.x.com/v1/chat/completions"]
        );
        // 常见：Base 已含 /v1（含尾斜杠）
        assert_eq!(
            c("https://api.deepseek.com/v1"),
            vec!["https://api.deepseek.com/v1/chat/completions"]
        );
        assert_eq!(
            c("https://api.deepseek.com/v1/"),
            vec!["https://api.deepseek.com/v1/chat/completions"]
        );
        // 缺 /v1：提供兜底候选（修复 404 的主要场景）
        assert_eq!(
            c("https://api.openai.com"),
            vec![
                "https://api.openai.com/chat/completions",
                "https://api.openai.com/v1/chat/completions"
            ]
        );
        // 本地带端口（Ollama / LM Studio 根地址）
        assert_eq!(
            c("http://127.0.0.1:11434"),
            vec![
                "http://127.0.0.1:11434/chat/completions",
                "http://127.0.0.1:11434/v1/chat/completions"
            ]
        );
        // 深路径末段是版本段 → 不重复补 /v1
        assert_eq!(
            c("https://openrouter.ai/api/v1"),
            vec!["https://openrouter.ai/api/v1/chat/completions"]
        );
        assert_eq!(
            c("https://api.commandcode.ai/provider/v1"),
            vec!["https://api.commandcode.ai/provider/v1/chat/completions"]
        );
        assert_eq!(
            c("https://open.bigmodel.cn/api/paas/v4"),
            vec!["https://open.bigmodel.cn/api/paas/v4/chat/completions"]
        );
    }

    /// 集成：Base URL 缺 /v1 时自动回退（/chat/completions 404 → /v1/chat/completions 成功）
    #[tokio::test]
    async fn falls_back_to_v1_when_missing() {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({
                    "choices": [{"message": {"content": "ok"}}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let llm = Llm::new(LlmSettings {
            provider: "openai".into(),
            base_url: format!("http://{addr}"), // 故意不带 /v1
            api_key: String::new(),
            model: "m".into(),
            temperature: 0.5,
        });
        let out = llm.complete("s", "u").await.unwrap();
        assert_eq!(out.text, "ok");
    }

    /// 集成：所有候选端点都 404 时，错误信息列出已尝试的端点（便于诊断）
    #[tokio::test]
    async fn reports_tried_endpoints_on_404() {
        let app = axum::Router::new(); // 无任何路由 → 全部 404
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let llm = Llm::new(LlmSettings {
            provider: "openai".into(),
            base_url: format!("http://{addr}"),
            api_key: String::new(),
            model: "m".into(),
            temperature: 0.5,
        });
        let err = format!("{}", llm.complete("s", "u").await.unwrap_err());
        assert!(err.contains("404"), "{err}");
        assert!(err.contains("已尝试端点"), "应列出所有已尝试的端点: {err}");
        assert!(err.contains(&format!("http://{addr}/chat/completions")), "{err}");
        assert!(err.contains(&format!("http://{addr}/v1/chat/completions")), "{err}");
    }

    // ---------- 模型列表拉取 ----------

    #[test]
    fn model_list_candidates_variants() {
        let c = |b: &str| model_list_candidates(b);
        assert_eq!(
            c("https://api.deepseek.com/v1"),
            vec!["https://api.deepseek.com/v1/models"]
        );
        assert_eq!(
            c("https://api.openai.com"),
            vec!["https://api.openai.com/models", "https://api.openai.com/v1/models"]
        );
        // 误填完整 chat 路径 → 自动剥掉再补 /models
        assert_eq!(
            c("https://api.deepseek.com/v1/chat/completions"),
            vec!["https://api.deepseek.com/v1/models"]
        );
        // 深路径（Zen Go / CommandCode）
        assert_eq!(
            c("https://opencode.ai/zen/go/v1"),
            vec![
                "https://opencode.ai/inference/go/v1/models",
                "https://opencode.ai/inference/go/openai/v1/models"
            ],
            "旧 Go 地址应归一化到官方 inference 前缀"
        );
        assert_eq!(
            c("https://api.commandcode.ai/provider/v1"),
            vec!["https://api.commandcode.ai/provider/v1/models"]
        );
    }

    /// 集成：拉取 OpenAI 兼容 /models 列表（排序去重；缺 /v1 自动回退）
    #[tokio::test]
    async fn lists_models_with_fallback() {
        let app = axum::Router::new().route(
            "/v1/models",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "object": "list",
                    "data": [
                        {"id": "model-b", "object": "model"},
                        {"id": "model-a", "object": "model"},
                        {"id": "model-b", "object": "model"}
                    ]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let llm = Llm::new(LlmSettings {
            provider: "openai".into(),
            base_url: format!("http://{addr}"), // 缺 /v1
            api_key: String::new(),
            model: String::new(),
            temperature: 0.0,
        });
        let models = llm.list_models().await.unwrap();
        assert_eq!(models, vec!["model-a".to_string(), "model-b".to_string()]);
    }

    /// 集成：/models 全 404 → 报错含已尝试端点；响应格式不对 → 明确报错
    #[tokio::test]
    async fn model_list_errors_are_clear() {
        // 全 404
        let app = axum::Router::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let llm = Llm::new(LlmSettings {
            provider: "openai".into(),
            base_url: format!("http://{addr}/v1"),
            api_key: String::new(),
            model: String::new(),
            temperature: 0.0,
        });
        let err = format!("{}", llm.list_models().await.unwrap_err());
        assert!(err.contains("不存在"), "{err}");

        // 200 但格式不对
        let app2 = axum::Router::new().route(
            "/v1/models",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"foo": "bar"})) }),
        );
        let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr2 = listener2.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener2, app2).await;
        });
        let llm2 = Llm::new(LlmSettings {
            provider: "openai".into(),
            base_url: format!("http://{addr2}/v1"),
            api_key: String::new(),
            model: String::new(),
            temperature: 0.0,
        });
        let err2 = format!("{}", llm2.list_models().await.unwrap_err());
        assert!(err2.contains("未解析到模型"), "{err2}");
    }

    // ---------- OpenCode（Zen/Go）：地址归一化 + 会话头 ----------

    #[test]
    fn normalize_open_code_go_base() {
        assert_eq!(
            normalize_base_url("https://opencode.ai/zen/go/v1"),
            "https://opencode.ai/inference/go/openai/v1"
        );
        assert_eq!(
            normalize_base_url("https://opencode.ai/zen/go/"),
            "https://opencode.ai/inference/go/openai/v1"
        );
        // 其它地址不受影响（含 Zen）
        assert_eq!(
            normalize_base_url("https://opencode.ai/zen/v1"),
            "https://opencode.ai/zen/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/v1"),
            "https://api.deepseek.com/v1"
        );
        // 归一化后端点解析正确
        assert_eq!(
            endpoint_candidates("https://opencode.ai/zen/go/v1"),
            vec!["https://opencode.ai/inference/go/openai/v1/chat/completions"]
        );
    }

    #[test]
    fn opencode_go_model_list_uses_inference_prefix() {
        // Go 的模型列表在 inference/go/v1/models（与 chat 的 /openai/v1 前缀不同）
        let c = model_list_candidates("https://opencode.ai/inference/go/openai/v1");
        assert_eq!(c[0], "https://opencode.ai/inference/go/v1/models");
        // 旧地址归一化后同样命中
        let c2 = model_list_candidates("https://opencode.ai/zen/go/v1");
        assert_eq!(c2[0], "https://opencode.ai/inference/go/v1/models");
    }

    /// 集成：OpenCode 域名的请求带 x-opencode-session 与会话 UA；其它域名不带
    /// （用 `http://opencode.ai@127.0.0.1:port` 的 userinfo 形式触发域名判定但实际打本地）
    #[tokio::test]
    async fn opencode_session_header_is_sent() {
        use axum::http::HeaderMap;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let seen2 = seen.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |headers: HeaderMap| {
                let seen = seen2.clone();
                async move {
                    let session = headers
                        .get("x-opencode-session")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    let ua = headers
                        .get("user-agent")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    seen.lock().unwrap().push((session, ua));
                    axum::Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        // ① OpenCode 域名（userinfo 技巧→实际连本地）应带会话头
        let llm = Llm::new(LlmSettings {
            provider: "openai".into(),
            base_url: format!("http://opencode.ai@{addr}/v1"),
            api_key: "sk-test".into(),
            model: "m".into(),
            temperature: 0.5,
        });
        llm.complete("s", "u").await.unwrap();
        {
            let got = seen.lock().unwrap();
            assert_eq!(got.len(), 1);
            assert!(!got[0].0.is_empty(), "应发送 x-opencode-session");
            assert!(got[0].1.contains("android-AI-Novel-Agent"), "UA={}", got[0].1);
        }

        // ② 普通域名不应带该头
        let llm2 = Llm::new(LlmSettings {
            provider: "openai".into(),
            base_url: format!("http://{addr}/v1"),
            api_key: String::new(),
            model: "m".into(),
            temperature: 0.5,
        });
        llm2.complete("s", "u").await.unwrap();
        {
            let got = seen.lock().unwrap();
            assert_eq!(got.len(), 2);
            assert!(got[1].0.is_empty(), "普通服务商不应发送 x-opencode-session");
        }
    }
}
