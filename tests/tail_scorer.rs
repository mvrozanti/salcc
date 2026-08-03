//! M3 side-channel tests: transcript model tolerance, tail reader torn-line
//! and only-new-content semantics plus rotation, discovery munge, Anthropic
//! request/response pure functions, pricing math, budget stop, metrics log
//! roundtrip, score cache in-flight dedup. The live test hits the real API:
//! #[ignore]d, runs only with ANTHROPIC_API_KEY set and SALCC_LIVE_TEST=1.

use std::fs::{self, FileTimes, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use salcc::config::dimensions::UserDimension;
use salcc::config::{Config, ScorerConfig};
use salcc::score::anthropic::{self, AnthropicScorer, Usage};
use salcc::score::cache::ScoreCache;
use salcc::stats::metrics::{MetricsLog, Record};
use salcc::tail::discover;
use salcc::tail::model::{self, TextBlock};
use salcc::tail::reader::TailReader;
use salcc::types::{ConvoContext, Facet, LatencyClass, ScoreRequest, Scorer, SourceId, SpanScore};

const FIXTURE: &str = include_str!("fixtures/transcript_sample.jsonl");

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("salcc-test-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn set_mtime(path: &Path, mtime: SystemTime) {
    let file = OpenOptions::new().append(true).open(path).unwrap();
    file.set_times(FileTimes::new().set_modified(mtime)).unwrap();
}

#[test]
fn model_extracts_exactly_the_text_blocks() {
    let blocks: Vec<TextBlock> = FIXTURE.lines().filter_map(model::text_blocks).collect();
    assert_eq!(
        blocks,
        vec![
            TextBlock {
                message_id: "msg_01".to_string(),
                block_text: "I will now run the tests to verify the fix works.".to_string(),
                uuid: "a2".to_string(),
            },
            TextBlock {
                message_id: "msg_03".to_string(),
                block_text:
                    "Run `cargo check` before committing; the config parser now rejects unknown keys."
                        .to_string(),
                uuid: "a5".to_string(),
            },
        ]
    );
}

#[test]
fn reader_skips_history_and_handles_torn_lines() {
    let dir = temp_dir("reader");
    let path = dir.join("session-a.jsonl");
    let lines: Vec<&str> = FIXTURE.lines().collect();

    let mut file = fs::File::create(&path).unwrap();
    for line in &lines[..3] {
        writeln!(file, "{line}").unwrap();
    }
    file.flush().unwrap();

    let mut reader = TailReader::attach(path.clone(), None).unwrap();
    assert!(reader.poll().is_empty());

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    for line in &lines[3..7] {
        writeln!(file, "{line}").unwrap();
    }
    file.flush().unwrap();
    assert!(reader.poll().is_empty());

    let (head, rest) = lines[7].split_at(40);
    write!(file, "{head}").unwrap();
    file.flush().unwrap();
    assert!(reader.poll().is_empty());

    writeln!(file, "{rest}").unwrap();
    file.flush().unwrap();
    let blocks = reader.poll();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].message_id, "msg_03");
    assert_eq!(blocks[0].uuid, "a5");
    assert!(reader.poll().is_empty());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn reader_rotates_to_newer_session_file() {
    let dir = temp_dir("rotate");
    let lines: Vec<&str> = FIXTURE.lines().collect();
    let first = dir.join("session-a.jsonl");
    fs::write(&first, format!("{}\n", lines[2])).unwrap();

    let mut reader = TailReader::attach(first.clone(), Some(dir.clone())).unwrap();
    assert!(reader.poll().is_empty());

    let second = dir.join("session-b.jsonl");
    fs::write(&second, format!("{}\n", lines[7])).unwrap();
    set_mtime(&second, SystemTime::now() + Duration::from_secs(60));

    let blocks = reader.poll();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].message_id, "msg_03");
    assert_eq!(reader.session_path(), &second);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn discover_munges_cwd_and_picks_newest() {
    let root = temp_dir("discover");
    let cwd = Path::new("/home/m/Projects/salcc");
    let project = discover::project_dir_in(&root, cwd);
    assert_eq!(project.file_name().unwrap(), "-home-m-Projects-salcc");

    fs::create_dir_all(&project).unwrap();
    assert!(discover::discover_session_in(&root, cwd).is_none());

    let old = project.join("old-session.jsonl");
    let new = project.join("new-session.jsonl");
    fs::write(&old, "{}\n").unwrap();
    fs::write(&new, "{}\n").unwrap();
    fs::write(project.join("notes.txt"), "ignored").unwrap();
    set_mtime(&old, SystemTime::now() - Duration::from_secs(120));
    set_mtime(&new, SystemTime::now());

    assert_eq!(discover::discover_session_in(&root, cwd).unwrap(), new);

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn watch_finds_session_created_after_spawn() {
    let root = temp_dir("watch");
    let cwd = Path::new("/tmp/watched-project");
    let project = discover::project_dir_in(&root, cwd);
    fs::create_dir_all(&project).unwrap();

    let after = SystemTime::now();
    let watch_root = root.clone();
    let watcher = tokio::spawn(async move {
        discover::watch_for_new_session_in(&watch_root, Path::new("/tmp/watched-project"), after)
            .await
    });

    tokio::time::sleep(Duration::from_millis(30)).await;
    let session = project.join("fresh-session.jsonl");
    fs::write(&session, "{}\n").unwrap();
    set_mtime(&session, after + Duration::from_secs(5));

    let found = tokio::time::timeout(Duration::from_secs(2), watcher)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found, session);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn request_body_matches_contract() {
    let dim = UserDimension {
        name: "jargon".into(),
        keywords: Vec::new(),
        color: None,
        llm_hint: Some("project-specific jargon terms".to_string()),
    };
    let ctx = ConvoContext {
        session_id: "sess-1".to_string(),
        recent_block_summaries: vec!["fixed the yaml parser".to_string()],
        seen_entities: Default::default(),
    };
    let body = anthropic::build_request_body(
        "claude-haiku-4-5",
        "The block text under scrutiny.",
        &ctx,
        std::slice::from_ref(&dim),
    );

    assert_eq!(body["model"], "claude-haiku-4-5");
    assert_eq!(body["max_tokens"], 1000);
    let system = body["system"].as_str().unwrap();
    assert!(system.starts_with("You mark spans in an AI coding agent's terminal output"));
    assert!(system.contains("precision over recall."));
    assert!(system.contains(
        "Also mark user dimension 'jargon' (project-specific jargon terms): use facet 'jargon'."
    ));
    assert_eq!(body["messages"][0]["role"], "user");
    let content = body["messages"][0]["content"].as_str().unwrap();
    assert!(content.contains("fixed the yaml parser"));
    assert!(content.ends_with("The block text under scrutiny."));
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");

    let schema = &body["output_config"]["format"]["schema"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"][0], "spans");
    let item = &schema["properties"]["spans"]["items"];
    assert_eq!(item["additionalProperties"], false);
    let facets: Vec<&str> = item["properties"]["facet"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        facets,
        ["action", "risk", "command", "uncertainty", "novelty", "jargon"]
    );
}

#[test]
fn response_quotes_resolve_to_byte_ranges() {
    let text = "First run cargo test locally. If it flakes, run cargo test again with RUST_LOG=debug. Never run rm -rf target on a hunch.";
    let payload = serde_json::json!({
        "spans": [
            {"quote": "cargo test", "occurrence": 2, "facet": "command", "weight": 0.8},
            {"quote": "rm -rf target", "facet": "risk", "weight": 1.7},
            {"quote": "not present anywhere", "facet": "action", "weight": 0.9},
            {"quote": "cargo test", "occurrence": 9, "facet": "command", "weight": 0.5},
            {"quote": "RUST_LOG=debug", "facet": "mystery-facet", "weight": 0.4}
        ]
    });
    let response = serde_json::json!({
        "id": "msg_live",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": payload.to_string()}],
        "usage": {"input_tokens": 1234, "output_tokens": 567}
    });

    let (spans, usage) = anthropic::parse_response(&response, text, &[]).unwrap();
    assert_eq!(usage.input_tokens, 1234);
    assert_eq!(usage.output_tokens, 567);

    assert_eq!(spans.len(), 2);
    let second = text.match_indices("cargo test").nth(1).unwrap().0;
    assert_eq!(spans[0].range, second..second + "cargo test".len());
    assert_eq!(spans[0].facet, Facet::Command);
    assert_eq!(spans[0].source, SourceId::Anthropic);
    let risk = text.find("rm -rf target").unwrap();
    assert_eq!(spans[1].range, risk..risk + "rm -rf target".len());
    assert_eq!(spans[1].facet, Facet::Risk);
    assert_eq!(spans[1].weight, 1.0);
}

#[test]
fn usage_math_matches_haiku_pricing() {
    let plain = anthropic::usage_usd(&Usage {
        input_tokens: 1234,
        output_tokens: 567,
        ..Default::default()
    });
    assert!((plain - 0.004069).abs() < 1e-9);

    let cached = anthropic::usage_usd(&Usage {
        input_tokens: 1000,
        output_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 1000,
    });
    assert!((cached - 0.0011).abs() < 1e-9);
}

#[tokio::test]
async fn budget_exhaustion_short_circuits_scoring() {
    let dir = temp_dir("budget");
    let metrics = MetricsLog::at(dir.join("metrics.jsonl"));
    let config = ScorerConfig {
        max_session_usd: 0.01,
        min_block_words: 3,
        ..Default::default()
    };
    let scorer = AnthropicScorer::with_api_key(
        "test-key-not-real".to_string(),
        &config,
        Vec::new(),
        metrics.clone(),
    )
    .with_api_url("http://127.0.0.1:9/unreachable".to_string());
    let ctx = ConvoContext::default();

    assert_eq!(scorer.name(), "anthropic");
    assert_eq!(scorer.latency_class(), LatencyClass::Async);

    let short = scorer
        .score(ScoreRequest {
            text: "too short",
            ctx: &ctx,
        })
        .await
        .unwrap();
    assert!(short.is_empty());

    scorer.note_spend(0.02);
    let text = "this block is easily long enough to be scored but the session budget is already exhausted";
    let spans = scorer.score(ScoreRequest { text, ctx: &ctx }).await.unwrap();
    assert!(spans.is_empty());
    assert!(metrics.read_all().is_empty());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn metrics_log_roundtrips_records() {
    let dir = temp_dir("metrics");
    let log = MetricsLog::at(dir.join("nested/metrics.jsonl"));
    let record = Record {
        ts: 1_753_000_000_000,
        session: "sess-1".to_string(),
        block_id: "abcd1234".to_string(),
        model: "claude-haiku-4-5".to_string(),
        input_tokens: 10,
        output_tokens: 20,
        usd: 0.00011,
        spans_returned: 2,
        cache_hit: false,
    };
    log.append(&record);
    let mut hit = record.clone();
    hit.cache_hit = true;
    hit.usd = 0.0;
    log.append(&hit);

    assert_eq!(log.read_all(), vec![record, hit]);

    let _ = fs::remove_dir_all(&dir);
}

async fn scoring_future(calls: &AtomicUsize) -> anyhow::Result<Vec<SpanScore>> {
    calls.fetch_add(1, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(30)).await;
    Ok(vec![SpanScore::new(
        0..9,
        Facet::Action,
        0.9,
        SourceId::Anthropic,
    )])
}

#[tokio::test]
async fn cache_dedupes_concurrent_same_key_requests() {
    let cache = ScoreCache::new();
    let calls = AtomicUsize::new(0);
    let text = "identical block text scored twice concurrently";

    let (first, second) = tokio::join!(
        cache.get_or_score("msg_a", 0, text, || scoring_future(&calls)),
        cache.get_or_score("msg_a", 1, text, || scoring_future(&calls)),
    );
    let (spans_first, hit_first) = first.unwrap();
    let (spans_second, hit_second) = second.unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(Arc::ptr_eq(&spans_first, &spans_second));
    assert_eq!(usize::from(hit_first) + usize::from(hit_second), 1);

    let (spans_third, hit_third) = cache
        .get_or_score("msg_b", 0, text, || scoring_future(&calls))
        .await
        .unwrap();
    assert!(hit_third);
    assert!(Arc::ptr_eq(&spans_first, &spans_third));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(cache.len().await, 1);

    assert!(cache.get_by_block("msg_a", 0).await.is_some());
    assert!(cache.get_by_block("msg_a", 1).await.is_some());
    assert!(cache.get_by_block("msg_b", 0).await.is_some());
    assert!(cache.get_by_block("msg_a", 7).await.is_none());
    assert!(cache.get(text).await.is_some());
}

#[tokio::test]
async fn cache_does_not_cache_errors() {
    let cache = ScoreCache::new();
    let failed = cache
        .get_or_score("msg_e", 0, "text that fails scoring", || async {
            anyhow::bail!("scorer exploded")
        })
        .await;
    assert!(failed.is_err());

    let (spans, hit) = cache
        .get_or_score("msg_e", 0, "text that fails scoring", || async {
            Ok(Vec::new())
        })
        .await
        .unwrap();
    assert!(!hit);
    assert!(spans.is_empty());
}

#[test]
fn config_scorer_section_parses_and_defaults() {
    let default = Config::default();
    assert_eq!(default.scorer, ScorerConfig::default());
    assert_eq!(default.scorer.name, "self");
    assert_eq!(default.scorer.model, "claude-haiku-4-5");
    assert_eq!(default.scorer.api_key_env, "ANTHROPIC_API_KEY");
    assert_eq!(default.scorer.api_key_cmd, None);
    assert_eq!(default.scorer.max_session_usd, 1.0);
    assert_eq!(default.scorer.min_block_words, 20);
    assert!(default.scorer.inject_instruction);

    let named = Config::from_yaml("scorer: ollama\n").unwrap();
    assert_eq!(named.scorer.name, "ollama");
    assert_eq!(named.scorer.model, "claude-haiku-4-5");

    let table = Config::from_yaml(
        "scorer:\n  api_key_cmd: pass show anthropic\n  max_session_usd: 0.5\n  min_block_words: 10\n",
    )
    .unwrap();
    assert_eq!(table.scorer.name, "self");
    assert_eq!(table.scorer.model, "claude-haiku-4-5");
    assert_eq!(table.scorer.api_key_cmd.as_deref(), Some("pass show anthropic"));
    assert_eq!(table.scorer.max_session_usd, 0.5);
    assert_eq!(table.scorer.min_block_words, 10);
}

#[tokio::test]
#[ignore]
async fn live_haiku_scores_fixed_paragraph() {
    if std::env::var("SALCC_LIVE_TEST").as_deref() != Ok("1")
        || std::env::var("ANTHROPIC_API_KEY").is_err()
    {
        return;
    }
    let dir = temp_dir("live");
    let metrics = MetricsLog::at(dir.join("metrics.jsonl"));
    let scorer = AnthropicScorer::new(&ScorerConfig::default(), Vec::new(), metrics.clone()).unwrap();
    let text = "You must set the DATABASE_URL environment variable yourself before deploying because the agent cannot access your secrets manager. Then run `docker compose up -d` to start the stack. Warning: running `docker system prune -af` permanently deletes all unused images and volumes on this host.";
    let ctx = ConvoContext {
        session_id: "live-test".to_string(),
        ..Default::default()
    };

    let spans = scorer.score(ScoreRequest { text, ctx: &ctx }).await.unwrap();

    assert!(!spans.is_empty());
    for span in &spans {
        assert!(span.range.start < span.range.end);
        assert!(span.range.end <= text.len());
        assert!(text.is_char_boundary(span.range.start));
        assert!(text.is_char_boundary(span.range.end));
        assert!((0.0..=1.0).contains(&span.weight));
        assert_eq!(span.source, SourceId::Anthropic);
    }
    assert_eq!(metrics.read_all().len(), 1);

    let _ = fs::remove_dir_all(&dir);
}
