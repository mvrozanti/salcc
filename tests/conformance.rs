//! Conformance suite for the M1 shadow grid and finalization detector.
//!
//! Fixtures are asciinema cast v2 files (JSON header line, then
//! `[time, "o"|"r", data]` event lines) generated deterministically by the
//! builders in this file and committed under tests/fixtures/. Each test
//! parses the on-disk fixture with a tiny serde_json loader and drift-checks
//! it against the generator; set SALCC_REGEN_FIXTURES=1 to rewrite after
//! changing a builder.
//!
//! Three properties are verified per fixture:
//! - the final shadow grid state matches an insta snapshot;
//! - zero false finals: every row the finalizer ever classified
//!   ScrollbackBound keeps its generation untouched for the rest of the
//!   replay, across coarse, 7-byte, and (for ink) 1-byte chunkings, with
//!   burst ends driven by fixture timestamp gaps;
//! - the chunked shadow grid equals a reference vt100 parser fed the same
//!   bytes at once, including telemetry's cursor model and scroll counter
//!   against vt100's actual scrollback length.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use salcc::term::finalize::Finalizer;
use salcc::term::grid::ShadowGrid;
use salcc::types::{AbsRow, Finality};

const BURST_GAP_MS: u64 = 100;
const SPINNER: [char; 6] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴'];

#[derive(Debug, Clone, PartialEq)]
enum Action {
    Out(String),
    Resize { cols: u16, rows: u16 },
}

#[derive(Debug, Clone, PartialEq)]
struct CastEvent {
    t_ms: u64,
    action: Action,
}

#[derive(Debug, Clone, PartialEq)]
struct Cast {
    width: u16,
    height: u16,
    events: Vec<CastEvent>,
}

impl Cast {
    fn serialize(&self) -> String {
        let mut out = format!(
            "{{\"version\": 2, \"width\": {}, \"height\": {}}}\n",
            self.width, self.height
        );
        for event in &self.events {
            let t = event.t_ms as f64 / 1000.0;
            let line = match &event.action {
                Action::Out(data) => serde_json::to_string(&(t, "o", data)),
                Action::Resize { cols, rows } => {
                    serde_json::to_string(&(t, "r", format!("{cols}x{rows}")))
                }
            }
            .expect("cast event serializes");
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

fn parse_cast(text: &str) -> Cast {
    let mut lines = text.lines();
    let header: serde_json::Value =
        serde_json::from_str(lines.next().expect("cast has header")).expect("valid header json");
    let width = u16::try_from(header["width"].as_u64().expect("width")).expect("width fits u16");
    let height = u16::try_from(header["height"].as_u64().expect("height")).expect("height fits");
    let mut events = Vec::new();
    for line in lines {
        let (t, kind, data): (f64, String, String) =
            serde_json::from_str(line).expect("valid event json");
        let t_ms = (t * 1000.0).round() as u64;
        let action = match kind.as_str() {
            "o" => Action::Out(data),
            "r" => {
                let (cols, rows) = data.split_once('x').expect("resize is COLSxROWS");
                Action::Resize {
                    cols: cols.parse().expect("resize cols"),
                    rows: rows.parse().expect("resize rows"),
                }
            }
            other => panic!("unsupported cast event kind {other:?}"),
        };
        events.push(CastEvent { t_ms, action });
    }
    Cast {
        width,
        height,
        events,
    }
}

struct CastBuilder {
    width: u16,
    height: u16,
    t_ms: u64,
    events: Vec<CastEvent>,
}

impl CastBuilder {
    fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            t_ms: 0,
            events: Vec::new(),
        }
    }

    fn out(&mut self, data: &str) {
        self.t_ms += 1;
        self.events.push(CastEvent {
            t_ms: self.t_ms,
            action: Action::Out(data.to_owned()),
        });
    }

    fn gap(&mut self) {
        self.t_ms += 250;
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.t_ms += 1;
        self.events.push(CastEvent {
            t_ms: self.t_ms,
            action: Action::Resize { cols, rows },
        });
    }

    fn build(self) -> Cast {
        Cast {
            width: self.width,
            height: self.height,
            events: self.events,
        }
    }
}

fn plain() -> Cast {
    let mut cast = CastBuilder::new(80, 24);
    for i in 1..=60u32 {
        let line = if i % 7 == 0 {
            format!("line {i:02} {}\r\n", "abcdefghij".repeat(12))
        } else if i % 11 == 0 {
            format!("line {i:02}\tcol\ttab\tstops\r\n")
        } else {
            format!("line {i:02} the quick brown fox jumps over the lazy dog\r\n")
        };
        cast.out(&line);
        if i % 10 == 0 {
            cast.gap();
        }
    }
    cast.build()
}

struct InkSim {
    cast: CastBuilder,
    buf: String,
    row: u16,
    frame_top: u16,
    frame_h: u16,
    tick: usize,
    committed: u32,
}

impl InkSim {
    const BOTTOM: u16 = 23;

    fn new() -> Self {
        Self {
            cast: CastBuilder::new(80, 24),
            buf: String::new(),
            row: 0,
            frame_top: 0,
            frame_h: 0,
            tick: 0,
            committed: 0,
        }
    }

    fn flush(&mut self) {
        if !self.buf.is_empty() {
            let data = std::mem::take(&mut self.buf);
            self.cast.out(&data);
        }
    }

    fn gap(&mut self) {
        self.flush();
        self.cast.gap();
    }

    fn newline(&mut self) {
        self.buf.push_str("\r\n");
        if self.row == Self::BOTTOM {
            self.frame_top = self.frame_top.saturating_sub(1);
        } else {
            self.row += 1;
        }
    }

    fn commit_line(&mut self) {
        self.committed += 1;
        let n = self.committed;
        self.buf
            .push_str(&format!("static line {n:02} — committed output"));
        self.newline();
    }

    fn erase_frame(&mut self) {
        if self.frame_h > 0 {
            let up = self.row - self.frame_top;
            self.buf.push('\r');
            if up > 0 {
                self.buf.push_str(&format!("\x1b[{up}A"));
            }
            self.row = self.frame_top;
        }
        self.buf.push_str("\x1b[J");
        self.frame_h = 0;
    }

    fn draw_frame(&mut self, h: u16) {
        self.buf.push_str("\x1b[?2026h");
        self.frame_top = self.row;
        let glyph = SPINNER[self.tick % SPINNER.len()];
        for i in 0..h {
            if i > 0 {
                self.newline();
            }
            match i {
                0 => self
                    .buf
                    .push_str(&format!("{glyph} Working on task {:02}…", self.tick)),
                _ if i + 1 == h => self.buf.push_str("╰ esc to interrupt"),
                _ => self.buf.push_str(&format!("│ step {i} of {h}")),
            }
        }
        self.buf.push_str("\x1b[?2026l");
        self.frame_h = h;
        self.tick += 1;
    }
}

fn ink() -> Cast {
    let mut sim = InkSim::new();
    for _ in 0..5 {
        sim.commit_line();
    }
    sim.draw_frame(3);
    for round in 1..=20u32 {
        let repaint_h = if round % 5 == 0 { 5 } else { 3 };
        for _ in 0..3 {
            sim.gap();
            sim.erase_frame();
            sim.draw_frame(repaint_h);
        }
        sim.gap();
        sim.erase_frame();
        sim.commit_line();
        sim.commit_line();
        sim.draw_frame(3);
    }
    sim.gap();
    sim.erase_frame();
    sim.buf.push_str("done — 45 static lines committed\r\n");
    sim.flush();
    sim.cast.build()
}

fn altscreen() -> Cast {
    let mut cast = CastBuilder::new(80, 24);
    for i in 1..=30u32 {
        cast.out(&format!("primary scrolling line {i:02}\r\n"));
        if i % 10 == 0 {
            cast.gap();
        }
    }
    cast.gap();
    cast.out("\x1b[?1049h\x1b[2J");
    for pass in 1..=3u32 {
        cast.gap();
        let mut frame = String::from("\x1b[H");
        for r in 1..=24u32 {
            frame.push_str(&format!("\x1b[{r};1H\x1b[Kalt row {r:02} pass {pass}"));
        }
        cast.out(&frame);
    }
    cast.gap();
    cast.out("\x1b[?1049l");
    cast.out("back on primary line 1\r\n");
    cast.out("back on primary line 2\r\n");
    cast.build()
}

fn styled() -> Cast {
    let mut cast = CastBuilder::new(80, 24);
    cast.out("\x1b[1;31mbold red\x1b[0m \x1b[3;32mitalic green\x1b[0m \x1b[4;38;5;208munderline 256\x1b[0m\r\n");
    cast.out("\x1b[38;2;255;100;0;48;2;20;20;80mtruecolor on navy\x1b[0m \x1b[2mdim\x1b[0m \x1b[7minverse\x1b[0m \x1b[9mstrike\x1b[0m\r\n");
    cast.out("cjk: 漢字テスト 中文宽字符 ラベル\r\n");
    cast.out("emoji: 🚀 rocket ✨ sparkle 🎉 party\r\n");
    cast.out(&format!("{}漢 wide char at wrap boundary\r\n", "a".repeat(79)));
    for i in 1..=30u32 {
        cast.out(&format!(
            "\x1b[38;5;{}mstyled filler line {i:02} with color\x1b[0m\r\n",
            16 + i
        ));
        if i % 8 == 0 {
            cast.gap();
        }
    }
    cast.gap();
    cast.resize(100, 30);
    cast.out("after resize: a wider line that now fits in one hundred columns without wrap\r\n");
    cast.out("\x1b[1;35mafter resize bold magenta\x1b[0m and 全角文字 again 🚀\r\n");
    cast.build()
}

fn fixture(name: &str, generate: fn() -> Cast) -> Cast {
    static FIXTURE_IO: Mutex<()> = Mutex::new(());
    let _guard = FIXTURE_IO.lock().expect("fixture lock");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.cast"));
    let generated = generate().serialize();
    if std::env::var_os("SALCC_REGEN_FIXTURES").is_some() || !path.exists() {
        fs::create_dir_all(path.parent().expect("fixture dir")).expect("create fixtures dir");
        fs::write(&path, &generated).expect("write fixture");
    }
    let disk = fs::read_to_string(&path).expect("read fixture");
    assert_eq!(
        disk, generated,
        "{name}.cast drifted from its generator; rerun with SALCC_REGEN_FIXTURES=1"
    );
    parse_cast(&disk)
}

fn fixtures() -> Vec<(&'static str, Cast)> {
    vec![
        ("plain", fixture("plain", plain)),
        ("ink", fixture("ink", ink)),
        ("altscreen", fixture("altscreen", altscreen)),
        ("styled", fixture("styled", styled)),
    ]
}

fn replay(
    cast: &Cast,
    chunk_size: usize,
    mut after_chunk: impl FnMut(&ShadowGrid, &Finalizer),
) -> (ShadowGrid, Finalizer) {
    let mut grid = ShadowGrid::new(cast.height, cast.width);
    let mut fin = Finalizer::new(cast.height);
    let mut last_ms: Option<u64> = None;
    for event in &cast.events {
        if let Some(last) = last_ms
            && event.t_ms.saturating_sub(last) > BURST_GAP_MS
        {
            fin.note_burst_end();
            grid.telemetry_mut().end_burst();
        }
        last_ms = Some(event.t_ms);
        match &event.action {
            Action::Out(data) => {
                for chunk in data.as_bytes().chunks(chunk_size.max(1)) {
                    grid.process(chunk);
                    let events = grid.telemetry_mut().take_events();
                    fin.ingest(events);
                    after_chunk(&grid, &fin);
                }
            }
            Action::Resize { cols, rows } => {
                grid.resize(*rows, *cols);
                fin.resize(*rows);
                after_chunk(&grid, &fin);
            }
        }
    }
    fin.note_burst_end();
    grid.telemetry_mut().end_burst();
    (grid, fin)
}

fn render_state(grid: &ShadowGrid) -> String {
    let (rows, cols) = grid.size();
    let (cur_row, cur_col) = grid.cursor();
    format!(
        "{}\n---\nsize: {rows}x{cols} cursor: ({cur_row},{cur_col}) scroll_base: {} alt: {}\n",
        grid.contents(),
        grid.scroll_base(),
        grid.alt_screen_active()
    )
}

struct FinalLedger {
    recorded: BTreeMap<u64, u64>,
    visible_finals: usize,
}

impl FinalLedger {
    fn new() -> Self {
        Self {
            recorded: BTreeMap::new(),
            visible_finals: 0,
        }
    }

    fn sweep(&mut self, grid: &ShadowGrid, fin: &Finalizer) {
        let (rows, _) = grid.size();
        let horizon = fin.scroll_base() + u64::from(rows);
        for abs in 0..horizon {
            if fin.classify(AbsRow(abs)) == Finality::ScrollbackBound
                && !self.recorded.contains_key(&abs)
            {
                if abs >= fin.scroll_base() {
                    self.visible_finals += 1;
                }
                self.recorded.insert(abs, grid.row_generation(AbsRow(abs)));
            }
        }
    }

    fn assert_no_false_finals(&self, name: &str, chunk_size: usize, grid: &ShadowGrid) {
        for (abs, generation) in &self.recorded {
            assert_eq!(
                grid.row_generation(AbsRow(*abs)),
                *generation,
                "false final in {name} (chunk {chunk_size}): row {abs} rewritten after ScrollbackBound"
            );
        }
    }
}

fn check_zero_false_finals(name: &str, cast: &Cast, chunk_size: usize) -> FinalLedger {
    let mut ledger = FinalLedger::new();
    let (grid, fin) = replay(cast, chunk_size, |grid, fin| ledger.sweep(grid, fin));
    ledger.sweep(&grid, &fin);
    ledger.assert_no_false_finals(name, chunk_size, &grid);
    ledger
}

#[test]
fn grid_snapshot_plain() {
    let cast = fixture("plain", plain);
    let (grid, _fin) = replay(&cast, 4096, |_, _| {});
    insta::assert_snapshot!("plain", render_state(&grid));
}

#[test]
fn grid_snapshot_ink() {
    let cast = fixture("ink", ink);
    let (grid, _fin) = replay(&cast, 4096, |_, _| {});
    insta::assert_snapshot!("ink", render_state(&grid));
}

#[test]
fn grid_snapshot_altscreen() {
    let cast = fixture("altscreen", altscreen);
    let (grid, _fin) = replay(&cast, 4096, |_, _| {});
    insta::assert_snapshot!("altscreen", render_state(&grid));
}

#[test]
fn grid_snapshot_styled() {
    let cast = fixture("styled", styled);
    let (grid, _fin) = replay(&cast, 4096, |_, _| {});
    insta::assert_snapshot!("styled", render_state(&grid));
}

#[test]
fn zero_false_finals_all_fixtures() {
    let floors: BTreeMap<&str, usize> = [
        ("plain", 25),
        ("ink", 15),
        ("altscreen", 5),
        ("styled", 5),
    ]
    .into();
    for (name, cast) in fixtures() {
        for chunk_size in [4096, 7] {
            let ledger = check_zero_false_finals(name, &cast, chunk_size);
            assert!(
                ledger.recorded.len() >= floors[name],
                "{name} finalized only {} rows; detector is uselessly conservative",
                ledger.recorded.len()
            );
        }
    }
}

#[test]
fn zero_false_finals_ink_byte_by_byte() {
    let cast = fixture("ink", ink);
    let ledger = check_zero_false_finals("ink", &cast, 1);
    assert!(
        ledger.recorded.len() >= 15,
        "ink finalized only {} rows byte-by-byte",
        ledger.recorded.len()
    );
    assert!(
        ledger.visible_finals >= 5,
        "ink finalized only {} rows while still visible; dynamic-region detection is not working",
        ledger.visible_finals
    );
}

#[test]
fn matches_reference_vt100() {
    for (name, cast) in fixtures() {
        for chunk_size in [4096, 3] {
            let (grid, _fin) = replay(&cast, chunk_size, |_, _| {});
            let mut reference = vt100::Parser::new(cast.height, cast.width, 10_000);
            let mut pending: Vec<u8> = Vec::new();
            for event in &cast.events {
                match &event.action {
                    Action::Out(data) => pending.extend_from_slice(data.as_bytes()),
                    Action::Resize { cols, rows } => {
                        reference.process(&pending);
                        pending.clear();
                        reference.screen_mut().set_size(*rows, *cols);
                    }
                }
            }
            reference.process(&pending);
            assert_eq!(
                grid.contents(),
                reference.screen().contents(),
                "{name} (chunk {chunk_size}): grid text diverged from reference"
            );
            assert_eq!(
                grid.cursor(),
                reference.screen().cursor_position(),
                "{name} (chunk {chunk_size}): cursor diverged from reference"
            );
            assert_eq!(
                grid.telemetry().cursor(),
                reference.screen().cursor_position(),
                "{name} (chunk {chunk_size}): telemetry cursor model diverged from vt100"
            );
            assert_eq!(
                grid.alt_screen_active(),
                reference.screen().alternate_screen(),
                "{name} (chunk {chunk_size}): alt-screen flag diverged"
            );
            reference.screen_mut().set_scrollback(usize::MAX);
            let reference_scrollback = reference.screen().scrollback();
            reference.screen_mut().set_scrollback(0);
            assert_eq!(
                grid.scroll_base(),
                reference_scrollback as u64,
                "{name} (chunk {chunk_size}): scroll counter diverged from vt100 scrollback"
            );
        }
    }
}

#[test]
fn row_text_reaches_scrollback_and_viewport() {
    let cast = fixture("plain", plain);
    let (mut grid, _fin) = replay(&cast, 4096, |_, _| {});
    assert!(grid.scroll_base() > 0, "plain fixture must scroll");
    assert_eq!(
        grid.row_text(AbsRow(0)).as_deref(),
        Some("line 01 the quick brown fox jumps over the lazy dog")
    );
    assert_eq!(
        grid.row_text(AbsRow(1)).as_deref(),
        Some("line 02 the quick brown fox jumps over the lazy dog")
    );
    let top_visible = grid.abs_row_of_visible(0);
    assert_eq!(grid.visible_row_of(top_visible), Some(0));
    assert_eq!(top_visible.0, grid.scroll_base());
    assert!(grid.row_text(top_visible).is_some());
    let beyond = AbsRow(grid.scroll_base() + 100);
    assert_eq!(grid.row_text(beyond), None);
    assert_eq!(grid.visible_row_of(beyond), None);

    let cast = fixture("ink", ink);
    let (mut grid, _fin) = replay(&cast, 4096, |_, _| {});
    assert_eq!(
        grid.row_text(AbsRow(0)).as_deref(),
        Some("static line 01 — committed output")
    );
}
