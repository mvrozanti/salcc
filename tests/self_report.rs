//! M6 self-annotation scorer suite. Verifies: annotation extraction is
//! byte-preserving (blanked text keeps every offset valid into the
//! original), facet code mapping including user dimensions and malformed
//! entry drops, SelfScorer end-to-end span resolution with fixed weights
//! and SourceId::SelfReport, cross-block quote resolution attributing spans
//! to the message block that contains the quote, emission-time concealment
//! of annotation-only lines (SGR 8+2, text bytes untouched, heuristics
//! skipped), the config default flip to scorer "self" with
//! inject_instruction parsing, and an offline fixture replay proving the
//! annotation line renders concealed through the full StylingTransform.

use std::path::PathBuf;

use salcc::config::{Config, ScorerConfig};
use salcc::emit::pipeline::StylingTransform;
use salcc::emit::styler::{CONCEAL_DIM_SGR, Styler};
use salcc::proxy::io_loop::ChunkTransform;
use salcc::score::heuristic::HeuristicScorer;
use salcc::score::self_report::{
    ACTION_WEIGHT, RISK_WEIGHT, SELF_ANNOTATION_INSTRUCTION, SelfScorer, UNCERTAINTY_WEIGHT,
    extract_annotations, is_annotation_line, resolve_against_message,
};
use salcc::term::caps::Caps;
use salcc::term::telemetry::SgrState;
use salcc::types::{ConvoContext, Facet, ScoreRequest, Scorer, SourceId};

const ANNOTATION: &str = "⟦r:git reset --hard❙a:snapshot the database first❙u:might not preserve ownership⟧";

#[test]
fn extract_parses_multi_entry_annotation() {
    let text = format!("prose before\n{ANNOTATION}");
    let (blanked, entries) = extract_annotations(&text);
    assert_eq!(blanked.len(), text.len());
    assert_eq!(
        entries,
        vec![
            (Facet::Risk, "git reset --hard".to_string()),
            (Facet::Action, "snapshot the database first".to_string()),
            (Facet::Uncertainty, "might not preserve ownership".to_string()),
        ]
    );
    assert!(blanked.starts_with("prose before\n"));
    assert!(blanked[12..].trim().is_empty());
}

#[test]
fn extract_maps_unknown_code_to_user_facet() {
    let (_, entries) = extract_annotations("x\n⟦security:leaked token value⟧");
    assert_eq!(
        entries,
        vec![(Facet::User("security".into()), "leaked token value".to_string())]
    );
}

#[test]
fn extract_skips_malformed_entries() {
    let (_, entries) = extract_annotations("x\n⟦nocolon❙r:ab❙a:long enough quote❙:empty code⟧");
    assert_eq!(entries, vec![(Facet::Action, "long enough quote".to_string())]);
}

#[test]
fn extract_blanks_mid_text_annotation_byte_preserving() {
    let text = "alpha ⟦r:alpha span here⟧ omega";
    let (blanked, entries) = extract_annotations(text);
    assert_eq!(blanked.len(), text.len());
    assert_eq!(entries.len(), 1);
    assert!(blanked.starts_with("alpha "));
    assert!(blanked.ends_with(" omega"));
    assert!(!blanked.contains('⟦'));
    let omega = blanked.find("omega").unwrap();
    assert_eq!(omega, text.find("omega").unwrap());
}

#[tokio::test]
async fn self_scorer_resolves_spans_into_prose() {
    let prose = "Snapshot the database first, then git reset --hard.\nThis might not preserve ownership.\n";
    let text = format!(
        "{prose}⟦r:git reset --hard❙a:Snapshot the database first❙u:might not preserve ownership❙c:absent from prose⟧"
    );
    let scorer = SelfScorer::new(Vec::new());
    assert_eq!(scorer.name(), "self");
    let ctx = ConvoContext::default();
    let spans = scorer
        .score(ScoreRequest { text: &text, ctx: &ctx })
        .await
        .unwrap();
    assert_eq!(spans.len(), 3);
    for span in &spans {
        assert_eq!(span.source, SourceId::SelfReport);
    }
    let risk = &spans[0];
    assert_eq!(risk.facet, Facet::Risk);
    assert_eq!(risk.weight, RISK_WEIGHT);
    assert_eq!(&text[risk.range.clone()], "git reset --hard");
    let action = &spans[1];
    assert_eq!(action.facet, Facet::Action);
    assert_eq!(action.weight, ACTION_WEIGHT);
    assert_eq!(&text[action.range.clone()], "Snapshot the database first");
    let uncertainty = &spans[2];
    assert_eq!(uncertainty.facet, Facet::Uncertainty);
    assert_eq!(uncertainty.weight, UNCERTAINTY_WEIGHT);
    assert_eq!(&text[uncertainty.range.clone()], "might not preserve ownership");
}

#[test]
fn cross_block_quote_resolves_to_earlier_block() {
    let block0 = "I will run cargo nextest and check the results.".to_string();
    let (block1, entries) = extract_annotations("All checks passed.\n⟦c:cargo nextest⟧");
    let blocks = vec![block0.clone(), block1];
    let resolved = resolve_against_message(&blocks, &entries);
    assert_eq!(resolved.len(), 1);
    let (block_idx, span) = &resolved[0];
    assert_eq!(*block_idx, 0);
    assert_eq!(span.facet, Facet::Command);
    assert_eq!(&block0[span.range.clone()], "cargo nextest");
    assert_eq!(span.source, SourceId::SelfReport);
}

#[test]
fn cross_block_prefers_current_block() {
    let blocks = vec![
        "shared phrase lives here".to_string(),
        "shared phrase lives here too".to_string(),
    ];
    let entries = vec![(Facet::Novelty, "shared phrase".to_string())];
    let resolved = resolve_against_message(&blocks, &entries);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].0, 1);
}

#[test]
fn cross_block_unresolvable_quote_drops() {
    let blocks = vec!["nothing matches".to_string()];
    let entries = vec![(Facet::Risk, "missing quote".to_string())];
    assert!(resolve_against_message(&blocks, &entries).is_empty());
}

fn default_styler() -> Styler {
    let config = Config::default();
    Styler::new(
        config.theme.clone(),
        Caps::truecolor_undercurl(),
        config.density_budget,
    )
}

fn parse_row(bytes: &[u8]) -> String {
    let mut parser = vt100::Parser::new(2, 120, 0);
    parser.process(bytes);
    parser.screen().rows(0, 120).next().unwrap_or_default()
}

#[test]
fn annotation_line_is_concealed_attrs_only() {
    let styler = default_styler();
    let raw = format!("{ANNOTATION}\r\n");
    let styled = styler
        .style_line(raw.as_bytes(), &SgrState::default(), true, |_| {
            panic!("annotation line must not reach span scoring")
        })
        .expect("annotation line styles");
    let text = String::from_utf8_lossy(&styled.bytes).into_owned();
    assert!(styled.bytes.starts_with(CONCEAL_DIM_SGR), "missing conceal+dim enter: {text:?}");
    assert!(text.contains(ANNOTATION), "text bytes changed: {text:?}");
    assert!(text.contains("\x1b[0m"), "missing SGR restore: {text:?}");
    assert!(!styled.used_reverse);
    assert_eq!(parse_row(&styled.bytes), ANNOTATION);
}

#[test]
fn normal_line_never_takes_conceal_path() {
    let styler = default_styler();
    let scorer = HeuristicScorer::new(&Config::default());
    let raw = b"  git push --force origin release\r\n";
    let styled = styler
        .style_line(raw, &SgrState::default(), true, |clean| scorer.score_line(clean))
        .expect("risk line styles");
    assert!(
        !styled
            .bytes
            .windows(CONCEAL_DIM_SGR.len())
            .any(|w| w == CONCEAL_DIM_SGR),
        "conceal sequence leaked into a normal line"
    );
}

#[test]
fn heuristic_scorer_ignores_annotation_lines() {
    let scorer = HeuristicScorer::new(&Config::default());
    assert!(scorer.score_line("⟦r:rm -rf /tmp/build❙c:git push --force⟧").is_empty());
    assert!(is_annotation_line("  ⟦r:padded annotation line⟧  "));
    assert!(!is_annotation_line("prose then ⟦r:not alone on line⟧"));
    assert!(!is_annotation_line("⟦r:unterminated"));
}

#[test]
fn config_defaults_to_self_scorer() {
    let default = Config::default();
    assert_eq!(default.scorer.name, "self");
    assert!(default.scorer.inject_instruction);
    assert_eq!(ScorerConfig::default().name, "self");

    let anthropic = Config::from_yaml("scorer: anthropic\n").unwrap();
    assert_eq!(anthropic.scorer.name, "anthropic");

    let table = Config::from_yaml("scorer:\n  name: self\n  inject_instruction: false\n").unwrap();
    assert_eq!(table.scorer.name, "self");
    assert!(!table.scorer.inject_instruction);
}

#[test]
fn instruction_mentions_format_and_codes() {
    for needle in ["⟦", "❙", "⟧", "VERBATIM", "0-3 marks"] {
        assert!(
            SELF_ANNOTATION_INSTRUCTION.contains(needle),
            "instruction lost {needle:?}"
        );
    }
}

#[test]
fn fixture_replay_conceals_annotation_line() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/self_annotation.cast");
    let text = std::fs::read_to_string(&path).expect("fixture readable");
    let mut lines = text.lines();
    lines.next().expect("header");
    let config = Config::default();
    let mut transform = StylingTransform::new(&config, Caps::truecolor_undercurl(), 24, 80);
    let mut out = Vec::new();
    let mut last_t: Option<f64> = None;
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let (t, kind, data): (f64, String, String) = serde_json::from_str(line).expect("event");
        if last_t.is_some_and(|last| t - last > 0.1) {
            transform.note_virtual_quiet(&mut out);
        }
        last_t = Some(t);
        if kind == "o" {
            transform.transform(data.as_bytes(), &mut out);
        }
    }
    transform.finish(&mut out);
    let rendered = String::from_utf8_lossy(&out).into_owned();
    let conceal_at = rendered.find("\u{1b}[8;2m").expect("conceal sequence emitted");
    let annotation_at = rendered.find("⟦r:hard-reset the working tree⟧").expect("annotation text intact");
    assert!(conceal_at < annotation_at, "conceal must precede the annotation text");
    let mut parser = vt100::Parser::new(24, 80, 100);
    parser.process(&out);
    let rows: Vec<String> = parser.screen().rows(0, 80).collect();
    assert!(rows.iter().any(|r| r.contains("⟦r:hard-reset the working tree⟧")));
}
