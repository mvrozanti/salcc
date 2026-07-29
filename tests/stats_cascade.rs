//! M4 acceptance: cascade merge semantics (same-facet max weight, veto
//! policy with the Risk hard invariant, disjoint passthrough, deterministic
//! order), metrics aggregation over a known fixture (2 sessions, 2 days,
//! cache hits — exact totals), the stats HTTP surface on an ephemeral port,
//! and llm_can_veto config parsing (risk override ignored). USD fixture
//! values are binary-exact (0.25 / 0.5) so sums compare with ==.

use std::path::PathBuf;

use salcc::config::{Config, VetoPolicy};
use salcc::score::cascade;
use salcc::stats::metrics::{MetricsLog, Record};
use salcc::stats::server;
use salcc::types::{Facet, SourceId, SpanScore};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("salcc-m4-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn heuristic(range: std::ops::Range<usize>, facet: Facet, weight: f32) -> SpanScore {
    SpanScore::new(range, facet, weight, SourceId::Heuristic)
}

fn llm(range: std::ops::Range<usize>, facet: Facet, weight: f32) -> SpanScore {
    SpanScore::new(range, facet, weight, SourceId::Anthropic)
}

#[test]
fn cascade_same_facet_overlap_max_weight_wins() {
    let policy = VetoPolicy::default();
    let merged = cascade::merge(
        &[heuristic(0..10, Facet::Command, 0.75)],
        &[llm(5..15, Facet::Command, 0.9)],
        &policy,
    );
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].source, SourceId::Anthropic);
    assert_eq!(merged[0].weight, 0.9);

    let merged = cascade::merge(
        &[heuristic(0..10, Facet::Command, 0.95)],
        &[llm(5..15, Facet::Command, 0.6)],
        &policy,
    );
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].source, SourceId::Heuristic);
    assert_eq!(merged[0].weight, 0.95);
}

#[test]
fn cascade_llm_veto_on_uncertainty_honored() {
    let merged = cascade::merge(
        &[heuristic(0..10, Facet::Uncertainty, 0.4)],
        &[llm(2..8, Facet::Uncertainty, 0.0)],
        &VetoPolicy::default(),
    );
    assert!(merged.is_empty(), "vetoed heuristic and the veto span itself must both vanish");
}

#[test]
fn cascade_veto_on_risk_ignored() {
    let merged = cascade::merge(
        &[heuristic(0..10, Facet::Risk, 0.95)],
        &[llm(0..10, Facet::Risk, 0.0)],
        &VetoPolicy::default(),
    );
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].facet, Facet::Risk);
    assert_eq!(merged[0].source, SourceId::Heuristic);
    assert_eq!(merged[0].weight, 0.95);
}

#[test]
fn cascade_policy_can_forbid_veto_per_facet() {
    let policy = VetoPolicy {
        uncertainty: false,
        ..VetoPolicy::default()
    };
    let merged = cascade::merge(
        &[heuristic(0..10, Facet::Uncertainty, 0.4)],
        &[llm(2..8, Facet::Uncertainty, 0.0)],
        &policy,
    );
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].source, SourceId::Heuristic);
}

#[test]
fn cascade_disjoint_spans_pass_through() {
    let merged = cascade::merge(
        &[heuristic(0..5, Facet::Risk, 0.95)],
        &[llm(10..15, Facet::Novelty, 0.8)],
        &VetoPolicy::default(),
    );
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].facet, Facet::Risk);
    assert_eq!(merged[1].facet, Facet::Novelty);
}

#[test]
fn cascade_cross_facet_overlap_keeps_both() {
    let merged = cascade::merge(
        &[heuristic(0..10, Facet::Command, 0.75)],
        &[llm(5..15, Facet::Action, 0.9)],
        &VetoPolicy::default(),
    );
    assert_eq!(merged.len(), 2);
}

#[test]
fn cascade_output_order_deterministic_by_range_start() {
    let heuristics = [
        heuristic(20..25, Facet::Command, 0.75),
        heuristic(0..5, Facet::Risk, 0.95),
    ];
    let llms = [
        llm(30..40, Facet::Novelty, 0.8),
        llm(8..12, Facet::Action, 0.7),
    ];
    let first = cascade::merge(&heuristics, &llms, &VetoPolicy::default());
    let second = cascade::merge(&heuristics, &llms, &VetoPolicy::default());
    let starts: Vec<usize> = first.iter().map(|s| s.range.start).collect();
    assert_eq!(starts, vec![0, 8, 20, 30]);
    let second_starts: Vec<usize> = second.iter().map(|s| s.range.start).collect();
    assert_eq!(starts, second_starts);
}

const DAY_A: u64 = 20454;
const DAY_B: u64 = 20455;
const MS_PER_DAY: u64 = 86_400_000;

fn record(ts: u64, session: &str, block: &str, tokens: (u64, u64), usd: f64, spans: usize, hit: bool) -> Record {
    Record {
        ts,
        session: session.to_string(),
        block_id: block.to_string(),
        model: "claude-haiku-4-5".to_string(),
        input_tokens: tokens.0,
        output_tokens: tokens.1,
        usd,
        spans_returned: spans,
        cache_hit: hit,
    }
}

fn fixture_records() -> Vec<Record> {
    vec![
        record(DAY_A * MS_PER_DAY + 3_600_000, "sess-a", "blk-1", (1000, 100), 0.25, 3, false),
        record(DAY_A * MS_PER_DAY + 4_600_000, "sess-a", "blk-1", (0, 0), 0.0, 3, true),
        record(DAY_B * MS_PER_DAY + 7_200_000, "sess-b", "blk-2", (2000, 200), 0.5, 5, false),
        record(DAY_B * MS_PER_DAY + 8_200_000, "sess-b", "blk-3", (500, 50), 0.25, 1, false),
    ]
}

fn write_fixture(tag: &str) -> MetricsLog {
    let log = MetricsLog::at(temp_dir(tag).join("metrics.jsonl"));
    for record in fixture_records() {
        log.append(&record);
    }
    log
}

#[test]
fn metrics_aggregation_exact_totals() {
    let log = write_fixture("agg");
    let records = log.read_all();
    assert_eq!(records.len(), 4);

    let summary = server::summarize(&records);
    assert_eq!(summary.total_usd, 1.0);
    assert_eq!(summary.total_calls, 4);
    assert_eq!(summary.api_calls, 3);
    assert_eq!(summary.cache_hits, 1);
    assert_eq!(summary.cache_hit_rate, 0.25);
    assert_eq!(summary.input_tokens, 3500);
    assert_eq!(summary.output_tokens, 350);
    assert_eq!(summary.spans, 12);
    assert_eq!(summary.sessions, 2);

    assert_eq!(summary.days.len(), 2);
    assert_eq!(summary.days[0].day, "2026-01-01");
    assert_eq!(summary.days[0].calls, 2);
    assert_eq!(summary.days[0].usd, 0.25);
    assert_eq!(summary.days[0].spans, 6);
    assert_eq!(summary.days[1].day, "2026-01-02");
    assert_eq!(summary.days[1].calls, 2);
    assert_eq!(summary.days[1].usd, 0.75);
    assert_eq!(summary.days[1].spans, 6);

    let sessions = server::session_rollups(&records);
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].session, "sess-b");
    assert_eq!(sessions[0].calls, 2);
    assert_eq!(sessions[0].cache_hits, 0);
    assert_eq!(sessions[0].usd, 0.75);
    assert_eq!(sessions[0].spans, 6);
    assert_eq!(sessions[1].session, "sess-a");
    assert_eq!(sessions[1].calls, 2);
    assert_eq!(sessions[1].cache_hits, 1);
    assert_eq!(sessions[1].usd, 0.25);
    assert_eq!(sessions[1].input_tokens, 1000);
}

#[test]
fn metrics_table_renders_totals_and_rollups() {
    let log = write_fixture("table");
    let table = server::render_table(&log.read_all());
    assert!(table.contains("4 records"));
    assert!(table.contains("1.000000"));
    assert!(table.contains("25.0%"));
    assert!(table.contains("3500 / 350"));
    assert!(table.contains("sess-a"));
    assert!(table.contains("sess-b"));
    assert!(table.contains("2026-01-01"));
    assert!(table.contains("2026-01-02"));
    assert!(server::render_table(&[]).contains("no scoring metrics"));
}

#[tokio::test]
async fn http_endpoints_serve_fixture_aggregates() {
    let log = write_fixture("http");
    let bound = server::bind(log, 0).await.unwrap();
    let addr = bound.addr;
    let running = tokio::spawn(bound.run());
    let client = reqwest::Client::new();

    let summary: serde_json::Value = client
        .get(format!("http://{addr}/api/summary"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(summary["total_usd"], 1.0);
    assert_eq!(summary["total_calls"], 4);
    assert_eq!(summary["api_calls"], 3);
    assert_eq!(summary["cache_hits"], 1);
    assert_eq!(summary["cache_hit_rate"], 0.25);
    assert_eq!(summary["spans"], 12);
    assert_eq!(summary["days"][0]["day"], "2026-01-01");
    assert_eq!(summary["days"][1]["usd"], 0.75);

    let sessions: serde_json::Value = client
        .get(format!("http://{addr}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sessions.as_array().unwrap().len(), 2);
    assert_eq!(sessions[0]["session"], "sess-b");
    assert_eq!(sessions[0]["usd"], 0.75);
    assert_eq!(sessions[1]["session"], "sess-a");
    assert_eq!(sessions[1]["cache_hits"], 1);

    let session_a: serde_json::Value = client
        .get(format!("http://{addr}/api/session/sess-a"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let records = session_a.as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1]["cache_hit"], true);
    assert_eq!(records[1]["usd"], 0.0);
    assert_eq!(records[1]["input_tokens"], 0);

    let html = client
        .get(format!("http://{addr}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("salcc"));
    assert!(html.contains("/api/summary"));

    running.abort();
}

#[test]
fn config_llm_can_veto_overrides_parse() {
    let config = Config::from_yaml(
        "llm_can_veto:\n  uncertainty: false\n  novelty: false\n  todo: false\n",
    )
    .unwrap();
    let veto = &config.llm_can_veto;
    assert!(!veto.uncertainty);
    assert!(!veto.novelty);
    assert!(veto.action);
    assert!(veto.command);
    assert!(veto.dim);
    assert!(!veto.allows(&Facet::User("todo".into())));
    assert!(veto.allows(&Facet::User("unlisted".into())));
}

#[test]
fn config_risk_veto_override_is_ignored() {
    let config = Config::from_yaml("llm_can_veto:\n  risk: true\n").unwrap();
    assert!(
        !config.llm_can_veto.allows(&Facet::Risk),
        "risk veto is a hard invariant; yaml must not be able to enable it"
    );
    let default = Config::default();
    assert!(!default.llm_can_veto.allows(&Facet::Risk));
    assert!(default.llm_can_veto.allows(&Facet::Uncertainty));
    assert!(default.llm_can_veto.allows(&Facet::Dim));
}
