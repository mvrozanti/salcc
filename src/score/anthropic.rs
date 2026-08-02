//! Async Haiku scorer over the Anthropic Messages API. Raw reqwest POST,
//! structured output (json_schema) so the model returns verbatim quotes,
//! never offsets. Quotes resolve to byte ranges here via memmem substring
//! search (quote + 1-based occurrence); an unresolvable quote drops its span
//! (fail-safe invariant #4). Request/response shaping and quote resolution
//! are pure functions so tests never touch the network.
//!
//! Cost discipline: blocks under min_block_words are skipped; every API call
//! appends a stats metrics record; cumulative usd is tracked and once
//! max_session_usd is exhausted the scorer returns Ok(vec![]) forever,
//! logging the stop exactly once. Errors: one retry on 429/5xx/transport
//! with 200-500ms jittered backoff, then Err. 10s request timeout.

use std::ops::Range;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::ScorerConfig;
use crate::config::dimensions::UserDimension;
use crate::stats::metrics::{MetricsLog, Record};
use crate::types::{ConvoContext, Facet, LatencyClass, ScoreFuture, ScoreRequest, Scorer, SourceId, SpanScore};

pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const MAX_TOKENS: u32 = 1000;
pub const INPUT_USD_PER_MTOK: f64 = 1.00;
pub const OUTPUT_USD_PER_MTOK: f64 = 5.00;
const CACHE_WRITE_INPUT_MULTIPLIER: f64 = 1.25;
const CACHE_READ_INPUT_MULTIPLIER: f64 = 0.10;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const BUILTIN_FACETS: [&str; 5] = ["action", "risk", "command", "uncertainty", "novelty"];

pub const MARKING_PROMPT: &str = "You mark spans in an AI coding agent's terminal output so a busy expert can triage it without reading linearly. Mark ONLY what they must not miss. Most text gets no marks; at most one mark per ~40 words; return an empty spans list when nothing qualifies. Facets — action: something the USER must do themselves; risk: destructive, irreversible, or security-sensitive operations mentioned or performed; command: a literal runnable command worth copying; uncertainty: hedged or unverified claims; novelty: first mention of a new file, decision, or constraint in this session. Rules — quote each span VERBATIM (3-120 chars, findable by exact substring search in the text; add occurrence if the quote appears more than once); weight is the cost of the user missing it, 0 to 1; never mark headings, code fence markers, or boilerplate; when unsure, do not mark: precision over recall.";

pub fn system_prompt(dimensions: &[UserDimension]) -> String {
    let mut prompt = MARKING_PROMPT.to_string();
    for dim in dimensions {
        prompt.push(' ');
        match &dim.llm_hint {
            Some(hint) => prompt.push_str(&format!(
                "Also mark user dimension '{name}' ({hint}): use facet '{name}'.",
                name = dim.name,
            )),
            None => prompt.push_str(&format!(
                "Also mark user dimension '{name}': use facet '{name}'.",
                name = dim.name,
            )),
        }
    }
    prompt
}

pub fn spans_schema(dimensions: &[UserDimension]) -> Value {
    let mut facets: Vec<String> = BUILTIN_FACETS.iter().map(|f| f.to_string()).collect();
    for dim in dimensions {
        let name = dim.name.to_string();
        if !facets.contains(&name) {
            facets.push(name);
        }
    }
    json!({
        "type": "object",
        "properties": {
            "spans": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "quote": {"type": "string"},
                        "occurrence": {"type": "integer"},
                        "facet": {"type": "string", "enum": facets},
                        "weight": {"type": "number"}
                    },
                    "required": ["quote", "facet", "weight"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["spans"],
        "additionalProperties": false
    })
}

pub fn user_content(text: &str, ctx: &ConvoContext) -> String {
    if ctx.recent_block_summaries.is_empty() {
        return text.to_string();
    }
    let mut content =
        String::from("Context from earlier in this session (never quote from it):\n");
    for summary in &ctx.recent_block_summaries {
        content.push_str("- ");
        content.push_str(summary);
        content.push('\n');
    }
    content.push_str("\nText to mark:\n");
    content.push_str(text);
    content
}

pub fn build_request_body(
    model: &str,
    text: &str,
    ctx: &ConvoContext,
    dimensions: &[UserDimension],
) -> Value {
    json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": system_prompt(dimensions),
        "messages": [{"role": "user", "content": user_content(text, ctx)}],
        "output_config": {"format": {"type": "json_schema", "schema": spans_schema(dimensions)}}
    })
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

impl Usage {
    pub fn total_input_tokens(&self) -> u64 {
        self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }
}

pub fn usage_usd(usage: &Usage) -> f64 {
    let weighted_input = usage.input_tokens as f64
        + usage.cache_creation_input_tokens as f64 * CACHE_WRITE_INPUT_MULTIPLIER
        + usage.cache_read_input_tokens as f64 * CACHE_READ_INPUT_MULTIPLIER;
    (weighted_input * INPUT_USD_PER_MTOK + usage.output_tokens as f64 * OUTPUT_USD_PER_MTOK)
        / 1_000_000.0
}

fn estimate_call_cost(text: &str) -> f64 {
    let est_input_tokens = (MARKING_PROMPT.len() + text.len()) as f64 / 3.0;
    (est_input_tokens * INPUT_USD_PER_MTOK + f64::from(MAX_TOKENS) * OUTPUT_USD_PER_MTOK)
        / 1_000_000.0
}

#[derive(Debug, Deserialize)]
struct SpansPayload {
    #[serde(default)]
    spans: Vec<RawSpan>,
}

#[derive(Debug, Deserialize)]
struct RawSpan {
    quote: String,
    #[serde(default)]
    occurrence: Option<u64>,
    facet: String,
    weight: f64,
}

pub fn resolve_quote(text: &str, quote: &str, occurrence: u64) -> Option<Range<usize>> {
    if quote.is_empty() {
        return None;
    }
    let nth = occurrence.max(1) as usize - 1;
    let start = memchr::memmem::find_iter(text.as_bytes(), quote.as_bytes()).nth(nth)?;
    Some(start..start + quote.len())
}

fn facet_from_name(name: &str, dimensions: &[UserDimension]) -> Option<Facet> {
    match name {
        "action" => Some(Facet::Action),
        "risk" => Some(Facet::Risk),
        "command" => Some(Facet::Command),
        "uncertainty" => Some(Facet::Uncertainty),
        "novelty" => Some(Facet::Novelty),
        other => dimensions
            .iter()
            .find(|dim| dim.name == other)
            .map(|dim| Facet::User(dim.name.clone())),
    }
}

pub fn parse_response(
    response: &Value,
    block_text: &str,
    dimensions: &[UserDimension],
) -> anyhow::Result<(Vec<SpanScore>, Usage)> {
    let usage: Usage = response
        .get("usage")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    let payload_text = response
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        })
        .and_then(|b| b.get("text"))
        .and_then(Value::as_str)
        .context("response has no text content block")?;
    let payload: SpansPayload =
        serde_json::from_str(payload_text).context("structured output did not match schema")?;
    let mut spans = Vec::with_capacity(payload.spans.len());
    for raw in payload.spans {
        let Some(facet) = facet_from_name(&raw.facet, dimensions) else {
            tracing::debug!(facet = %raw.facet, "unknown facet from model; span dropped");
            continue;
        };
        let Some(range) = resolve_quote(block_text, &raw.quote, raw.occurrence.unwrap_or(1))
        else {
            tracing::debug!(quote = %raw.quote, "unresolvable quote; span dropped");
            continue;
        };
        spans.push(SpanScore::new(
            range,
            facet,
            raw.weight as f32,
            SourceId::Anthropic,
        ));
    }
    Ok((spans, usage))
}

pub fn resolve_api_key(config: &ScorerConfig) -> anyhow::Result<String> {
    if let Some(cmd) = &config.api_key_cmd {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .with_context(|| format!("api_key_cmd failed to run: {cmd}"))?;
        anyhow::ensure!(output.status.success(), "api_key_cmd exited nonzero: {cmd}");
        let key = String::from_utf8(output.stdout)?.trim().to_string();
        anyhow::ensure!(!key.is_empty(), "api_key_cmd produced empty output: {cmd}");
        return Ok(key);
    }
    let key = std::env::var(&config.api_key_env)
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .with_context(|| format!("api key env var {} not set", config.api_key_env))?;
    Ok(key)
}

pub struct AnthropicScorer {
    client: reqwest::Client,
    api_key: String,
    model: String,
    min_block_words: usize,
    max_session_usd: f64,
    dimensions: Vec<UserDimension>,
    metrics: MetricsLog,
    spent_usd: Mutex<f64>,
    budget_stop_logged: AtomicBool,
    api_url: String,
}

impl AnthropicScorer {
    pub fn new(
        config: &ScorerConfig,
        dimensions: Vec<UserDimension>,
        metrics: MetricsLog,
    ) -> anyhow::Result<Self> {
        let api_key = resolve_api_key(config)?;
        Ok(Self::with_api_key(api_key, config, dimensions, metrics))
    }

    pub fn with_api_key(
        api_key: String,
        config: &ScorerConfig,
        dimensions: Vec<UserDimension>,
        metrics: MetricsLog,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client construction is infallible with these options");
        Self {
            client,
            api_key,
            model: config.model.clone(),
            min_block_words: config.min_block_words,
            max_session_usd: config.max_session_usd,
            dimensions,
            metrics,
            spent_usd: Mutex::new(0.0),
            budget_stop_logged: AtomicBool::new(false),
            api_url: API_URL.to_string(),
        }
    }

    pub fn with_api_url(mut self, api_url: String) -> Self {
        self.api_url = api_url;
        self
    }

    pub fn spent_usd(&self) -> f64 {
        *self.spent_usd.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn note_spend(&self, usd: f64) {
        let mut spent = self.spent_usd.lock().unwrap_or_else(|e| e.into_inner());
        *spent += usd;
    }

    fn try_reserve(&self, estimate: f64) -> bool {
        let mut spent = self.spent_usd.lock().unwrap_or_else(|e| e.into_inner());
        if *spent + estimate > self.max_session_usd {
            drop(spent);
            if !self.budget_stop_logged.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    max_session_usd = self.max_session_usd,
                    spent_usd = self.spent_usd(),
                    "session LLM budget would be exceeded; scoring disabled for remainder of session"
                );
            }
            return false;
        }
        *spent += estimate;
        true
    }

    fn reconcile(&self, estimate: f64, actual: f64) {
        let mut spent = self.spent_usd.lock().unwrap_or_else(|e| e.into_inner());
        *spent += actual - estimate;
    }

    async fn score_block(&self, req: ScoreRequest<'_>) -> anyhow::Result<Vec<SpanScore>> {
        if req.text.split_whitespace().count() < self.min_block_words {
            return Ok(Vec::new());
        }
        let estimate = estimate_call_cost(req.text);
        if !self.try_reserve(estimate) {
            return Ok(Vec::new());
        }
        let body = build_request_body(&self.model, req.text, req.ctx, &self.dimensions);
        let response = match self.post_with_retry(&body).await {
            Ok(response) => response,
            Err(err) => {
                self.reconcile(estimate, 0.0);
                return Err(err);
            }
        };
        let (spans, usage) = match parse_response(&response, req.text, &self.dimensions) {
            Ok(parsed) => parsed,
            Err(err) => {
                self.reconcile(estimate, 0.0);
                return Err(err);
            }
        };
        let usd = usage_usd(&usage);
        self.reconcile(estimate, usd);
        self.metrics.append(&Record {
            ts: Record::now_ms(),
            session: req.ctx.session_id.clone(),
            block_id: block_id(req.text),
            model: self.model.clone(),
            input_tokens: usage.total_input_tokens(),
            output_tokens: usage.output_tokens,
            usd,
            spans_returned: spans.len(),
            cache_hit: false,
        });
        tracing::debug!(
            spans = spans.len(),
            input_tokens = usage.total_input_tokens(),
            output_tokens = usage.output_tokens,
            usd,
            "anthropic scoring call complete"
        );
        Ok(spans)
    }

    async fn post_with_retry(&self, body: &Value) -> anyhow::Result<Value> {
        match self.post_once(body).await {
            Ok(value) => Ok(value),
            Err(err) if err.retryable => {
                tracing::debug!(err = %err.message, "retryable scoring failure; backing off once");
                tokio::time::sleep(jittered_backoff()).await;
                self.post_once(body)
                    .await
                    .map_err(|err| anyhow::anyhow!(err.message))
            }
            Err(err) => Err(anyhow::anyhow!(err.message)),
        }
    }

    async fn post_once(&self, body: &Value) -> Result<Value, ApiError> {
        let response = self
            .client
            .post(&self.api_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|err| ApiError {
                retryable: true,
                message: format!("anthropic request failed: {err}"),
            })?;
        let status = response.status();
        if status.is_success() {
            return response.json().await.map_err(|err| ApiError {
                retryable: false,
                message: format!("anthropic response body unreadable: {err}"),
            });
        }
        let retryable = status.as_u16() == 429 || status.is_server_error();
        let detail = response.text().await.unwrap_or_default();
        Err(ApiError {
            retryable,
            message: format!("anthropic returned {status}: {detail}"),
        })
    }
}

struct ApiError {
    retryable: bool,
    message: String,
}

pub fn block_id(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex()[..16].to_string()
}

fn jittered_backoff() -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    Duration::from_millis(200 + nanos % 300)
}

impl Scorer for AnthropicScorer {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn latency_class(&self) -> LatencyClass {
        LatencyClass::Async
    }

    fn score<'a>(&'a self, req: ScoreRequest<'a>) -> ScoreFuture<'a> {
        Box::pin(self.score_block(req))
    }
}
