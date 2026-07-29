//! M2 styling suite. Verifies the emission-channel invariants end to end:
//! (1) ATTRS-ONLY — replaying every fixture through StylingTransform yields
//! a byte stream whose vt100 grid TEXT (visible + scrollback) is
//! cell-for-cell identical to the original, the audited gutter glyph swap
//! being the sole permitted diff; (2) NO_COLOR (injected via Caps, never env
//! mutation) makes the transform byte-perfect passthrough; (3) styler unit
//! behavior — risk hue + glyph on padded lines, density-budget drops,
//! child-SGR restoration, dim boilerplate; (4) a labeled corpus asserting
//! every emphasis heuristic rule holds the 0.85 precision floor and risk
//! recall is total (labels use letters R/C/A/U/D for
//! Risk/Command/Action/Uncertainty/Dim). Offline replay mirrors
//! `salcc replay`: cast timestamp gaps over 100ms drive burst ends,
//! wall-clock hold deadlines never fire, and verdicts settle at finish.

use std::path::PathBuf;

use salcc::config::Config;
use salcc::emit::pipeline::StylingTransform;
use salcc::emit::styler::Styler;
use salcc::proxy::io_loop::ChunkTransform;
use salcc::score::heuristic::{
    ACTION_PRECISION, COMMAND_PRECISION, HeuristicScorer, RISK_COMMAND_PRECISION,
    RISK_PROSE_PRECISION,
};
use salcc::term::caps::Caps;
use salcc::term::telemetry::SgrState;
use salcc::types::{Facet, SourceId, SpanScore};

const GLYPHS: [char; 4] = ['▶', '⚠', '❯', '◆'];
const QUIET_GAP_SECS: f64 = 0.1;

enum Action {
    Out(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

struct Cast {
    width: u16,
    height: u16,
    events: Vec<(f64, Action)>,
}

enum Emitted {
    Original(Vec<u8>),
    Styled(Vec<u8>),
    Resize { rows: u16, cols: u16 },
}

fn parse_cast(text: &str) -> Cast {
    let mut lines = text.lines();
    let header: serde_json::Value =
        serde_json::from_str(lines.next().expect("cast header")).expect("header json");
    let width = u16::try_from(header["width"].as_u64().expect("width")).expect("width u16");
    let height = u16::try_from(header["height"].as_u64().expect("height")).expect("height u16");
    let mut events = Vec::new();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let (t, kind, data): (f64, String, String) =
            serde_json::from_str(line).expect("event json");
        let action = match kind.as_str() {
            "o" => Action::Out(data.into_bytes()),
            "r" => {
                let (cols, rows) = data.split_once('x').expect("resize COLSxROWS");
                Action::Resize {
                    cols: cols.parse().expect("cols"),
                    rows: rows.parse().expect("rows"),
                }
            }
            other => panic!("unsupported cast event {other:?}"),
        };
        events.push((t, action));
    }
    Cast {
        width,
        height,
        events,
    }
}

fn fixture_paths() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("fixtures dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "cast"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no fixtures found in {dir:?}");
    paths
}

fn drive(cast: &Cast, caps: Caps) -> Vec<Emitted> {
    let config = Config::default();
    let mut transform = StylingTransform::new(&config, caps, cast.height, cast.width);
    let mut log = Vec::new();
    let mut out = Vec::new();
    let mut last_t: Option<f64> = None;
    for (t, action) in &cast.events {
        if last_t.is_some_and(|last| t - last > QUIET_GAP_SECS) {
            out.clear();
            transform.note_virtual_quiet(&mut out);
            log.push(Emitted::Styled(out.clone()));
        }
        last_t = Some(*t);
        match action {
            Action::Out(bytes) => {
                log.push(Emitted::Original(bytes.clone()));
                out.clear();
                transform.transform(bytes, &mut out);
                log.push(Emitted::Styled(out.clone()));
            }
            Action::Resize { cols, rows } => {
                out.clear();
                transform.resize(*rows, *cols, &mut out);
                log.push(Emitted::Styled(out.clone()));
                log.push(Emitted::Resize {
                    rows: *rows,
                    cols: *cols,
                });
            }
        }
    }
    out.clear();
    transform.finish(&mut out);
    log.push(Emitted::Styled(out.clone()));
    log
}

fn all_rows(parser: &mut vt100::Parser) -> Vec<String> {
    let (_, cols) = parser.screen().size();
    parser.screen_mut().set_scrollback(usize::MAX);
    let available = parser.screen().scrollback();
    let mut rows = Vec::new();
    for back in (1..=available).rev() {
        parser.screen_mut().set_scrollback(back);
        rows.push(parser.screen().rows(0, cols).next().unwrap_or_default());
    }
    parser.screen_mut().set_scrollback(0);
    rows.extend(parser.screen().rows(0, cols));
    rows
}

fn rows_equivalent(original: &str, styled: &str) -> bool {
    if original == styled {
        return true;
    }
    let mut orig = original.chars();
    let mut sty = styled.chars();
    let pad_ok = orig.next() == Some(' ') && orig.next() == Some(' ');
    let glyph_ok = sty.next().is_some_and(|c| GLYPHS.contains(&c)) && sty.next() == Some(' ');
    pad_ok && glyph_ok && orig.as_str() == sty.as_str()
}

#[test]
fn attrs_only_all_fixtures() {
    for path in fixture_paths() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let cast = parse_cast(&std::fs::read_to_string(&path).expect("read fixture"));
        let log = drive(&cast, Caps::truecolor_undercurl());
        let mut original = vt100::Parser::new(cast.height, cast.width, 10_000);
        let mut styled = vt100::Parser::new(cast.height, cast.width, 10_000);
        for entry in &log {
            match entry {
                Emitted::Original(bytes) => original.process(bytes),
                Emitted::Styled(bytes) => styled.process(bytes),
                Emitted::Resize { rows, cols } => {
                    original.screen_mut().set_size(*rows, *cols);
                    styled.screen_mut().set_size(*rows, *cols);
                }
            }
        }
        let original_rows = all_rows(&mut original);
        let styled_rows = all_rows(&mut styled);
        assert_eq!(
            original_rows.len(),
            styled_rows.len(),
            "{name}: row count diverged"
        );
        for (i, (orig, sty)) in original_rows.iter().zip(&styled_rows).enumerate() {
            assert!(
                rows_equivalent(orig, sty),
                "{name}: row {i} text diverged\n original: {orig:?}\n styled:   {sty:?}"
            );
        }
    }
}

#[test]
fn no_color_is_bit_perfect() {
    for path in fixture_paths() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let cast = parse_cast(&std::fs::read_to_string(&path).expect("read fixture"));
        let log = drive(&cast, Caps::disabled());
        let mut input = Vec::new();
        let mut output = Vec::new();
        for entry in &log {
            match entry {
                Emitted::Original(bytes) => input.extend_from_slice(bytes),
                Emitted::Styled(bytes) => output.extend_from_slice(bytes),
                Emitted::Resize { .. } => {}
            }
        }
        assert_eq!(input, output, "{name}: NO_COLOR output diverged from input");
    }
}

fn default_styler() -> Styler {
    let config = Config::default();
    Styler::new(
        config.theme.clone(),
        Caps::truecolor_undercurl(),
        config.density_budget,
    )
}

fn scorer() -> HeuristicScorer {
    HeuristicScorer::new(&Config::default())
}

fn parse_row(bytes: &[u8]) -> String {
    let mut parser = vt100::Parser::new(2, 120, 0);
    parser.process(bytes);
    parser.screen().rows(0, 120).next().unwrap_or_default()
}

#[test]
fn risk_line_gets_hue_and_glyph_when_padded() {
    let styler = default_styler();
    let scorer = scorer();
    let raw = b"  git push --force origin release\r\n";
    let styled = styler
        .style_line(raw, &SgrState::default(), true, |clean| scorer.score_line(clean))
        .expect("risk line styles");
    let text = String::from_utf8_lossy(&styled.bytes).into_owned();
    assert!(text.contains("38;2;213;94;0"), "missing risk hue: {text:?}");
    assert!(text.contains('⚠'), "missing risk glyph: {text:?}");
    assert!(styled.used_reverse, "weight 0.95 risk span should take the reverse slot");
    assert_eq!(parse_row(&styled.bytes), "⚠ git push --force origin release");
}

#[test]
fn glyph_swap_requires_two_space_pad() {
    let styler = default_styler();
    let scorer = scorer();
    let raw = b"never run rm -rf / on the host\r\n";
    let styled = styler
        .style_line(raw, &SgrState::default(), true, |clean| scorer.score_line(clean))
        .expect("risk line styles");
    let text = String::from_utf8_lossy(&styled.bytes).into_owned();
    assert!(!GLYPHS.iter().any(|g| text.contains(*g)), "glyph without pad: {text:?}");
    assert_eq!(parse_row(&styled.bytes), "never run rm -rf / on the host");
}

#[test]
fn density_budget_drops_lowest_weight_spans() {
    let styler = default_styler();
    let raw = b"alpha bravo charlie delta echo foxtrot golf\r\n";
    let spans = vec![
        SpanScore::new(0..5, Facet::Risk, 0.95, SourceId::Heuristic),
        SpanScore::new(6..11, Facet::Command, 0.75, SourceId::Heuristic),
        SpanScore::new(12..19, Facet::Action, 0.7, SourceId::Heuristic),
    ];
    let styled = styler
        .style_line(raw, &SgrState::default(), false, move |_| spans.clone())
        .expect("over-budget line still styles the survivors");
    let text = String::from_utf8_lossy(&styled.bytes).into_owned();
    assert!(text.contains("38;2;213;94;0"), "risk span must survive: {text:?}");
    assert!(!text.contains("38;2;0;158;115"), "command span should be dropped: {text:?}");
    assert!(!text.contains("38;2;0;114;178"), "action span should be dropped: {text:?}");
    assert_eq!(parse_row(&styled.bytes), "alpha bravo charlie delta echo foxtrot golf");
}

#[test]
fn styled_span_restores_child_sgr_state() {
    let styler = default_styler();
    let scorer = scorer();
    let raw = b"\x1b[31mplease avoid rm -rf / here today\r\n";
    let styled = styler
        .style_line(raw, &SgrState::default(), false, |clean| scorer.score_line(clean))
        .expect("risk line styles");
    let text = String::from_utf8_lossy(&styled.bytes).into_owned();
    assert!(text.contains("\x1b[0;31m"), "span exit must restore child red: {text:?}");
    assert_eq!(parse_row(&styled.bytes), "please avoid rm -rf / here today");
    let mut parser = vt100::Parser::new(2, 120, 0);
    parser.process(&styled.bytes);
    let tail_cell = parser.screen().cell(0, 25).expect("tail cell");
    assert_eq!(
        tail_cell.fgcolor(),
        vt100::Color::Idx(1),
        "text after the styled span must stay child-red"
    );
}

#[test]
fn boilerplate_opener_is_dimmed_only() {
    let styler = default_styler();
    let scorer = scorer();
    let raw = b"Let me check the config file first.\r\n";
    let styled = styler
        .style_line(raw, &SgrState::default(), false, |clean| scorer.score_line(clean))
        .expect("dim line styles");
    let text = String::from_utf8_lossy(&styled.bytes).into_owned();
    assert!(text.contains("\x1b[2m"), "missing dim: {text:?}");
    assert!(!text.contains("38;2;"), "dim must be hueless: {text:?}");
    assert!(!text.contains("\x1b[1"), "dim must not embolden: {text:?}");
    assert_eq!(parse_row(&styled.bytes), "Let me check the config file first.");
}

#[test]
fn child_fg_wins_over_non_risk_hue() {
    let styler = default_styler();
    let scorer = scorer();
    let raw = b"\x1b[33mRun `cargo build --release` first.\r\n";
    let styled = styler
        .style_line(raw, &SgrState::default(), false, |clean| scorer.score_line(clean))
        .expect("command line styles");
    let text = String::from_utf8_lossy(&styled.bytes).into_owned();
    assert!(
        !text.contains("38;2;0;158;115"),
        "command hue must yield to child fg: {text:?}"
    );
    assert!(text.contains("\x1b[1m"), "luminance is still ours: {text:?}");
}

fn facet_letter(facet: &Facet) -> Option<char> {
    match facet {
        Facet::Risk => Some('R'),
        Facet::Command => Some('C'),
        Facet::Action => Some('A'),
        Facet::Uncertainty => Some('U'),
        Facet::Dim => Some('D'),
        _ => None,
    }
}

fn corpus() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Run `rm -rf node_modules` to clear the cache.", "RC"),
        ("This will permanently delete all user data.", "R"),
        ("git push -f origin main rewrites remote history.", "R"),
        ("  git reset --hard HEAD~3", "RC"),
        ("Use `dd of=/dev/sda` carefully.", "RC"),
        ("DROP TABLE users; this cannot be undone.", "R"),
        ("sudo rm /etc/passwd would be catastrophic.", "R"),
        ("chmod -R 777 / breaks every permission boundary.", "R"),
        ("The operation is irreversible.", "R"),
        ("mkfs.ext4 /dev/sdb1 formats the drive.", "R"),
        ("Use --force-with-lease instead; it protects remote work.", ""),
        ("The log output was truncated for brevity.", ""),
        ("We reinforce the invariant with tests.", ""),
        ("Run `cargo build --release` first.", "C"),
        ("  $ git status", "C"),
        ("Then execute `npm install` in the project root.", "C"),
        ("  docker compose up -d", "C"),
        ("  git is a distributed version control system.", ""),
        ("The `config.yaml` file holds the retry settings.", ""),
        ("cargo is the Rust package manager.", ""),
        ("You need to restart the shell for the changes to apply.", "A"),
        ("Make sure to back up the database first.", "A"),
        ("Don't forget to reload the systemd daemon.", "A"),
        ("Next step: you should now update the schema version.", "A"),
        ("Manually verify the generated diff before merging.", "A"),
        ("Run the following commands in order.", "A"),
        ("Before you deploy, snapshot the volume.", "A"),
        ("Do you need to see the full backtrace?", ""),
        ("The script runs the following checks automatically.", ""),
        ("It manually retries the request on failure.", ""),
        ("This should work, but I have not tested it on musl.", "U"),
        ("The cache entry is probably stale by now.", "U"),
        ("I think the race lives in the reader thread.", "U"),
        ("Roughly 200ms of tail latency remains.", "U"),
        ("That code path is untested.", "U"),
        ("The mighty borrow checker prevents the bug.", ""),
        ("The tool may notify you twice.", ""),
        ("Let me check the config file first.", "D"),
        ("I'll start by reading the failing test.", "D"),
        ("Perfect! All 13 tests pass.", "D"),
        ("Now I need to wire the transform into the loop.", "D"),
        ("Great! The build succeeded on the first try.", "D"),
        ("First, a warning: the wipe cannot be undone.", "DR"),
        ("Sure, I can add that check.", "D"),
    ]
}

#[test]
fn heuristic_precision_floors_hold_on_corpus() {
    const {
        assert!(RISK_COMMAND_PRECISION >= 0.85);
        assert!(RISK_PROSE_PRECISION >= 0.85);
        assert!(COMMAND_PRECISION >= 0.85);
        assert!(ACTION_PRECISION >= 0.85);
    }
    let scorer = scorer();
    let corpus = corpus();
    assert!(corpus.len() >= 30, "corpus must stay >= 30 labeled snippets");
    for letter in ['R', 'C', 'A', 'U', 'D'] {
        let mut predicted = 0usize;
        let mut correct = 0usize;
        for (text, labels) in &corpus {
            let fired = scorer
                .score_line(text)
                .iter()
                .any(|s| facet_letter(&s.facet) == Some(letter));
            if fired {
                predicted += 1;
                if labels.contains(letter) {
                    correct += 1;
                } else {
                    eprintln!("false positive {letter} on: {text}");
                }
            }
        }
        assert!(predicted > 0, "{letter}: rule never fired on corpus");
        let precision = correct as f32 / predicted as f32;
        assert!(
            precision >= 0.85,
            "{letter}: precision {precision:.2} below floor ({correct}/{predicted})"
        );
    }
    let mut risk_hits = 0usize;
    let mut risk_labeled = 0usize;
    for (text, labels) in &corpus {
        if labels.contains('R') {
            risk_labeled += 1;
            if scorer.score_line(text).iter().any(|s| s.facet == Facet::Risk) {
                risk_hits += 1;
            } else {
                eprintln!("risk miss on: {text}");
            }
        }
    }
    assert_eq!(risk_hits, risk_labeled, "risk recall must be total on the corpus");
}

#[test]
fn replayed_demo_contains_styling() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo.cast");
    let cast = parse_cast(&std::fs::read_to_string(&path).expect("demo fixture"));
    let log = drive(&cast, Caps::truecolor_undercurl());
    let mut styled = Vec::new();
    for entry in &log {
        if let Emitted::Styled(bytes) = entry {
            styled.extend_from_slice(bytes);
        }
    }
    let text = String::from_utf8_lossy(&styled).into_owned();
    assert!(text.contains("38;2;213;94;0"), "demo replay lost risk hue");
    assert!(text.contains("38;2;0;114;178"), "demo replay lost action hue");
    assert!(text.contains("38;2;0;158;115"), "demo replay lost command hue");
    assert!(text.contains("4:3"), "demo replay lost undercurl");
    assert!(text.contains('⚠'), "demo replay lost the risk gutter glyph");
    assert!(text.contains("\x1b[2m"), "demo replay lost dim boilerplate");
}
