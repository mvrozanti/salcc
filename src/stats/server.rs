//! Cost dashboard over metrics.jsonl. Pure aggregation (summarize /
//! session_rollups / day_rollups) feeds both faces: `render_table` for the
//! `salcc stats` terminal summary and an axum server for `salcc stats
//! --serve`. The server binds 127.0.0.1 only (no auth by design), reads the
//! metrics file per request (no live coupling to the proxy, torn tails are
//! skipped by read_all), and serves a single embedded HTML page — GET /
//! (include_str! assets/index.html), /api/summary (totals + per-day
//! rollups), /api/sessions (per-session rollups, newest first),
//! /api/session/{id} (that session's raw records). `bind` accepts port 0
//! and exposes the chosen address so tests can spin an ephemeral instance.
//! Cache-hit records carry zero tokens and zero usd, so totals stay
//! API-truthful while hit rate reflects every scoring decision. Day keys
//! are UTC civil dates derived arithmetically — no calendar dependency.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Html;
use axum::routing::get;
use axum::Json;
use serde::Serialize;

use super::metrics::{MetricsLog, Record};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub total_usd: f64,
    pub total_calls: usize,
    pub api_calls: usize,
    pub cache_hits: usize,
    pub cache_hit_rate: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub spans: usize,
    pub sessions: usize,
    pub days: Vec<DayRollup>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionRollup {
    pub session: String,
    pub calls: usize,
    pub cache_hits: usize,
    pub usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub spans: usize,
    pub last_ts: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DayRollup {
    pub day: String,
    pub calls: usize,
    pub usd: f64,
    pub spans: usize,
}

pub fn summarize(records: &[Record]) -> Summary {
    let cache_hits = records.iter().filter(|r| r.cache_hit).count();
    let sessions: std::collections::HashSet<&str> =
        records.iter().map(|r| r.session.as_str()).collect();
    Summary {
        total_usd: records.iter().map(|r| r.usd).sum(),
        total_calls: records.len(),
        api_calls: records.len() - cache_hits,
        cache_hits,
        cache_hit_rate: if records.is_empty() {
            0.0
        } else {
            cache_hits as f64 / records.len() as f64
        },
        input_tokens: records.iter().map(|r| r.input_tokens).sum(),
        output_tokens: records.iter().map(|r| r.output_tokens).sum(),
        spans: records.iter().map(|r| r.spans_returned).sum(),
        sessions: sessions.len(),
        days: day_rollups(records),
    }
}

pub fn session_rollups(records: &[Record]) -> Vec<SessionRollup> {
    let mut by_session: BTreeMap<&str, SessionRollup> = BTreeMap::new();
    for record in records {
        let rollup = by_session
            .entry(record.session.as_str())
            .or_insert_with(|| SessionRollup {
                session: record.session.clone(),
                calls: 0,
                cache_hits: 0,
                usd: 0.0,
                input_tokens: 0,
                output_tokens: 0,
                spans: 0,
                last_ts: 0,
            });
        rollup.calls += 1;
        rollup.cache_hits += usize::from(record.cache_hit);
        rollup.usd += record.usd;
        rollup.input_tokens += record.input_tokens;
        rollup.output_tokens += record.output_tokens;
        rollup.spans += record.spans_returned;
        rollup.last_ts = rollup.last_ts.max(record.ts);
    }
    let mut rollups: Vec<SessionRollup> = by_session.into_values().collect();
    rollups.sort_by(|a, b| b.last_ts.cmp(&a.last_ts).then(a.session.cmp(&b.session)));
    rollups
}

pub fn day_rollups(records: &[Record]) -> Vec<DayRollup> {
    let mut by_day: BTreeMap<String, DayRollup> = BTreeMap::new();
    for record in records {
        let day = utc_day(record.ts);
        let rollup = by_day.entry(day.clone()).or_insert_with(|| DayRollup {
            day,
            calls: 0,
            usd: 0.0,
            spans: 0,
        });
        rollup.calls += 1;
        rollup.usd += record.usd;
        rollup.spans += record.spans_returned;
    }
    by_day.into_values().collect()
}

pub fn utc_day(ts_ms: u64) -> String {
    let (year, month, day) = civil_from_days((ts_ms / 86_400_000) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

pub fn run_table() -> String {
    render_table(&MetricsLog::open_default().read_all())
}

pub fn render_table(records: &[Record]) -> String {
    if records.is_empty() {
        return "no scoring metrics recorded yet\n".to_string();
    }
    let summary = summarize(records);
    let mut out = String::new();
    out.push_str(&format!("salcc scoring costs — {} records\n\n", summary.total_calls));
    out.push_str(&format!("  total usd       {:.6}\n", summary.total_usd));
    out.push_str(&format!("  api calls       {}\n", summary.api_calls));
    out.push_str(&format!(
        "  cache hits      {} ({:.1}%)\n",
        summary.cache_hits,
        summary.cache_hit_rate * 100.0
    ));
    out.push_str(&format!(
        "  tokens in/out   {} / {}\n",
        summary.input_tokens, summary.output_tokens
    ));
    out.push_str(&format!("  spans returned  {}\n", summary.spans));
    out.push_str(&format!("  sessions        {}\n\n", summary.sessions));
    out.push_str(&format!(
        "  {:<38} {:>6} {:>6} {:>10} {:>7}\n",
        "session", "calls", "hits", "usd", "spans"
    ));
    for rollup in session_rollups(records) {
        out.push_str(&format!(
            "  {:<38} {:>6} {:>6} {:>10.6} {:>7}\n",
            rollup.session, rollup.calls, rollup.cache_hits, rollup.usd, rollup.spans
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "  {:<12} {:>6} {:>10} {:>7}\n",
        "day", "calls", "usd", "spans"
    ));
    for rollup in &summary.days {
        out.push_str(&format!(
            "  {:<12} {:>6} {:>10.6} {:>7}\n",
            rollup.day, rollup.calls, rollup.usd, rollup.spans
        ));
    }
    out
}

pub struct BoundStats {
    pub addr: SocketAddr,
    listener: tokio::net::TcpListener,
    router: Router,
}

impl BoundStats {
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!(addr = %self.addr, "stats server listening");
        axum::serve(self.listener, self.router).await?;
        Ok(())
    }
}

pub async fn bind(log: MetricsLog, port: u16) -> anyhow::Result<BoundStats> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let addr = listener.local_addr()?;
    Ok(BoundStats {
        addr,
        listener,
        router: router(log),
    })
}

pub async fn serve(log: MetricsLog, port: u16) -> anyhow::Result<()> {
    bind(log, port).await?.run().await
}

pub fn router(log: MetricsLog) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/summary", get(api_summary))
        .route("/api/sessions", get(api_sessions))
        .route("/api/session/{id}", get(api_session))
        .with_state(log)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("assets/index.html"))
}

async fn api_summary(State(log): State<MetricsLog>) -> Json<Summary> {
    Json(summarize(&log.read_all()))
}

async fn api_sessions(State(log): State<MetricsLog>) -> Json<Vec<SessionRollup>> {
    Json(session_rollups(&log.read_all()))
}

async fn api_session(State(log): State<MetricsLog>, Path(id): Path<String>) -> Json<Vec<Record>> {
    Json(log.read_all().into_iter().filter(|r| r.session == id).collect())
}
