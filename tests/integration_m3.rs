//! M3 integration: the async scoring side channel wired into the live
//! styling pipeline. Uses a fake transcript session dir plus an injected
//! fake Scorer (no network): (1) a block scored while its lines are still
//! held merges LLM spans into emission styling; (2) a block scored while
//! its rows are visible and final repaints them in place (sync-update
//! wrapped, CUP-targeted); (3) a block scored only after its rows scrolled
//! out produces zero repaint bytes — no CUP anywhere; (4) a transform with
//! an attached-but-silent channel is byte-identical to one with no channel
//! across a full fixture replay (degradation to pre-integration styling).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use salcc::config::Config;
use salcc::emit::pipeline::{SessionSource, StylingTransform, spawn_side_channel};
use salcc::proxy::io_loop::ChunkTransform;
use salcc::tail::reader::TailReader;
use salcc::term::caps::Caps;
use salcc::types::{Facet, LatencyClass, ScoreFuture, ScoreRequest, Scorer, SourceId, SpanScore};

const BLOCK_TEXT: &str =
    "The deploy step reads the manifest and applies it to the staging cluster.";
const QUOTE: &str = "applies it to the staging cluster";
const LINE_ONE: &str = "The deploy step reads the\r\n";
const LINE_TWO: &str = "manifest and applies it to the staging cluster.\r\n";
const NOVELTY_HUE: &str = "38;2;204;121;167";

struct FakeScorer {
    target: String,
    spans: Vec<SpanScore>,
}

impl Scorer for FakeScorer {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn latency_class(&self) -> LatencyClass {
        LatencyClass::Async
    }

    fn score<'a>(&'a self, req: ScoreRequest<'a>) -> ScoreFuture<'a> {
        let spans = if req.text == self.target {
            self.spans.clone()
        } else {
            Vec::new()
        };
        Box::pin(std::future::ready(Ok(spans)))
    }
}

fn novelty_scorer() -> FakeScorer {
    let start = BLOCK_TEXT.find(QUOTE).expect("quote in block");
    FakeScorer {
        target: BLOCK_TEXT.to_string(),
        spans: vec![SpanScore::new(
            start..start + QUOTE.len(),
            Facet::Novelty,
            0.9,
            SourceId::Anthropic,
        )],
    }
}

fn temp_session(tag: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("salcc-m3-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let session = dir.join("session-x.jsonl");
    fs::write(&session, "").unwrap();
    (dir, session)
}

fn transcript_line(message_id: &str, uuid: &str, text: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "isSidechain": false,
        "uuid": uuid,
        "message": {"id": message_id, "content": [{"type": "text", "text": text}]}
    })
    .to_string()
}

fn append_line(path: &Path, line: &str) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
    file.flush().unwrap();
}

fn attach_fake_channel(session: PathBuf, transform: &mut StylingTransform) {
    let reader = TailReader::attach(session, None).unwrap();
    let rx = spawn_side_channel(SessionSource::Attached(reader), Box::new(novelty_scorer()));
    transform.attach_score_channel(rx);
}

fn wait_for_scored(transform: &mut StylingTransform, styled: &mut Vec<u8>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut out = Vec::new();
    while transform.scored_block_count() == 0 {
        assert!(
            Instant::now() < deadline,
            "side channel never delivered a scored block"
        );
        std::thread::sleep(Duration::from_millis(20));
        out.clear();
        transform.transform(b"", &mut out);
        styled.extend_from_slice(&out);
    }
}

fn contains_cup(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'H' {
                return true;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    false
}

#[test]
fn scored_block_merges_into_emission_styling() {
    let (dir, session) = temp_session("merge");
    let config = Config::default();
    let mut transform = StylingTransform::new(&config, Caps::truecolor_undercurl(), 6, 80);
    attach_fake_channel(session.clone(), &mut transform);

    append_line(&session, &transcript_line("msg_merge", "u1", BLOCK_TEXT));
    let mut styled = Vec::new();
    wait_for_scored(&mut transform, &mut styled);

    let mut out = Vec::new();
    for line in [LINE_ONE, LINE_TWO] {
        out.clear();
        transform.transform(line.as_bytes(), &mut out);
        styled.extend_from_slice(&out);
    }
    for i in 0..8 {
        out.clear();
        transform.transform(format!("filler line {i}\r\n").as_bytes(), &mut out);
        styled.extend_from_slice(&out);
    }
    out.clear();
    transform.finish(&mut out);
    styled.extend_from_slice(&out);

    let text = String::from_utf8_lossy(&styled).into_owned();
    assert!(
        text.contains(NOVELTY_HUE),
        "novelty hue missing from emission merge: {text:?}"
    );
    assert!(text.contains(QUOTE), "quoted text lost: {text:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn late_scored_block_repaints_visible_rows() {
    let (dir, session) = temp_session("repaint");
    let config = Config::default();
    let mut transform = StylingTransform::new(&config, Caps::truecolor_undercurl(), 8, 80);
    attach_fake_channel(session.clone(), &mut transform);

    let mut out = Vec::new();
    let mut pre = Vec::new();
    for line in [LINE_ONE, LINE_TWO] {
        out.clear();
        transform.transform(line.as_bytes(), &mut out);
        pre.extend_from_slice(&out);
    }
    out.clear();
    transform.transform(b"\x1b[8;1H\x1b[K", &mut out);
    pre.extend_from_slice(&out);
    out.clear();
    transform.note_virtual_quiet(&mut out);
    pre.extend_from_slice(&out);
    assert!(
        !String::from_utf8_lossy(&pre).contains(NOVELTY_HUE),
        "hue appeared before the block was scored"
    );

    append_line(&session, &transcript_line("msg_repaint", "u2", BLOCK_TEXT));
    let mut drained = Vec::new();
    wait_for_scored(&mut transform, &mut drained);
    assert_eq!(transform.repaint_pending_rows(), 1, "span should queue on one visible row");

    out.clear();
    transform.transform(b"\x1b[8;1H\x1b[K", &mut out);
    transform.note_virtual_quiet(&mut out);
    std::thread::sleep(Duration::from_millis(150));
    out.clear();
    transform.flush(&mut out);

    let text = String::from_utf8_lossy(&out).into_owned();
    assert!(text.contains("\x1b[?2026h"), "batch not sync-update wrapped: {text:?}");
    assert!(text.contains("\x1b[2;1H"), "no CUP to the scored row: {text:?}");
    assert!(text.contains(NOVELTY_HUE), "novelty hue missing from repaint: {text:?}");
    assert!(text.contains(QUOTE), "repaint lost the row text: {text:?}");
    assert_eq!(transform.repaint_pending_rows(), 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn late_block_never_repaints_scrolled_rows() {
    let (dir, session) = temp_session("scrolled");
    let config = Config::default();
    let mut transform = StylingTransform::new(&config, Caps::truecolor_undercurl(), 4, 80);
    attach_fake_channel(session.clone(), &mut transform);

    let mut out = Vec::new();
    for line in [LINE_ONE, LINE_TWO] {
        out.clear();
        transform.transform(line.as_bytes(), &mut out);
    }
    for i in 0..6 {
        out.clear();
        transform.transform(format!("filler line {i}\r\n").as_bytes(), &mut out);
    }
    out.clear();
    transform.note_virtual_quiet(&mut out);

    append_line(&session, &transcript_line("msg_late", "u3", BLOCK_TEXT));
    let mut drained = Vec::new();
    wait_for_scored(&mut transform, &mut drained);
    assert_eq!(
        transform.repaint_pending_rows(),
        1,
        "span should queue against the scrolled row before build_batch drops it"
    );

    std::thread::sleep(Duration::from_millis(150));
    out.clear();
    transform.flush(&mut out);

    assert!(
        !contains_cup(&out),
        "repaint targeted a scrolled-out row: {:?}",
        String::from_utf8_lossy(&out)
    );
    assert!(!String::from_utf8_lossy(&out).contains(NOVELTY_HUE));
    assert_eq!(transform.repaint_pending_rows(), 0);
    let _ = fs::remove_dir_all(&dir);
}

fn replay_fixture(transform: &mut StylingTransform) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo.cast");
    let text = fs::read_to_string(&path).expect("demo fixture");
    let mut lines = text.lines();
    lines.next().expect("cast header");
    let mut styled = Vec::new();
    let mut out = Vec::new();
    let mut last_t: Option<f64> = None;
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let (t, kind, data): (f64, String, String) = serde_json::from_str(line).expect("event");
        if last_t.is_some_and(|last| t - last > 0.1) {
            out.clear();
            transform.note_virtual_quiet(&mut out);
            styled.extend_from_slice(&out);
        }
        last_t = Some(t);
        if kind == "o" {
            out.clear();
            transform.transform(data.as_bytes(), &mut out);
            styled.extend_from_slice(&out);
        }
    }
    out.clear();
    transform.finish(&mut out);
    styled.extend_from_slice(&out);
    styled
}

#[test]
fn silent_channel_output_matches_channel_free_pipeline() {
    let config = Config::default();
    let caps = Caps::truecolor_undercurl();
    let mut plain = StylingTransform::new(&config, caps, 24, 80);
    let baseline = replay_fixture(&mut plain);

    let (tx, rx) = std::sync::mpsc::channel();
    let mut with_channel = StylingTransform::new(&config, caps, 24, 80);
    with_channel.attach_score_channel(rx);
    let attached = replay_fixture(&mut with_channel);
    drop(tx);

    assert_eq!(
        baseline, attached,
        "silent side channel must not perturb emission styling"
    );
}
