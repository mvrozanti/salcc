//! M3 mapper + repaint suite. Verifies: (1) normalize_markdown source maps
//! round-trip span ranges through marker/fence/link/header stripping;
//! (2) the wrap matrix — one paragraph soft-wrapped at 40/80/120 cells with
//! CJK, emoji, and a hard-split unbreakable path maps every span to the
//! exact (row, col) cells at each width, expectations derived from the test
//! wrapper's own bookkeeping, not the code under test; (3) gutter prefixes
//! strip and offset columns; (4) fail-safe drops — duplicated span text with
//! equal candidates drops, anchor-miss with a unique quote falls back and
//! maps, absent text drops; (5) the repaint engine against a live
//! ShadowGrid+Finalizer session — sync-update wrapping per caps, CUP only on
//! queued eligible rows, cursor restored, layering the batch onto a
//! reference vt100 leaves text identical and changes attrs only inside
//! overlay ranges, glyph swap only in a 2-space pad, and generation-changed
//! or scrolled-out rows emit nothing.

use std::ops::Range;

use salcc::config::theme::Theme;
use salcc::map::align::anchor;
use salcc::map::locate::{CellSpan, map_spans};
use salcc::map::normalize::normalize_markdown;
use salcc::term::caps::{Caps, ColorDepth};
use salcc::term::finalize::Finalizer;
use salcc::term::grid::ShadowGrid;
use salcc::term::repaint::RepaintEngine;
use salcc::types::{AbsRow, Facet, SourceId, SpanScore};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

fn project(map: &[usize], range: Range<usize>) -> Range<usize> {
    let start = map.partition_point(|&o| o < range.start);
    let end = map.partition_point(|&o| o < range.end);
    start..end
}

fn find_range(hay: &str, needle: &str) -> Range<usize> {
    let start = hay.find(needle).expect("needle present");
    start..start + needle.len()
}

#[test]
fn markdown_bold_span_round_trips() {
    let clean = "this is **risky** stuff";
    let (norm, map) = normalize_markdown(clean);
    assert_eq!(norm, "this is risky stuff");
    let projected = project(&map, find_range(clean, "**risky**"));
    assert_eq!(&norm[projected], "risky");
}

#[test]
fn markdown_fence_lines_vanish_and_code_stays_verbatim() {
    let clean = "before\n```rust\nlet x = a * b;\n```\nafter";
    let (norm, map) = normalize_markdown(clean);
    assert_eq!(norm, "before let x = a * b; after");
    let projected = project(&map, find_range(clean, "a * b"));
    assert_eq!(&norm[projected], "a * b");
}

#[test]
fn markdown_link_keeps_text_drops_url() {
    let clean = "see [the docs](https://example.com/x) for details";
    let (norm, map) = normalize_markdown(clean);
    assert_eq!(norm, "see the docs for details");
    let projected = project(&map, find_range(clean, "the docs"));
    assert_eq!(&norm[projected], "the docs");
}

#[test]
fn markdown_headers_and_quotes_strip() {
    let clean = "# Title here\n> quoted words\n> > nested quote\nplain";
    let (norm, map) = normalize_markdown(clean);
    assert_eq!(norm, "Title here quoted words nested quote plain");
    let projected = project(&map, find_range(clean, "quoted words"));
    assert_eq!(&norm[projected], "quoted words");
}

#[test]
fn markdown_collapses_whitespace_and_trims() {
    let clean = "  leading\t\tand   interior\n\n\nruns  ";
    let (norm, _) = normalize_markdown(clean);
    assert_eq!(norm, "leading and interior runs");
}

struct Wrapped {
    rows: Vec<(AbsRow, String)>,
    positions: Vec<Option<(u64, u16)>>,
}

fn wrap(rendered: &str, width: u16) -> Wrapped {
    let word_width = |s: &str| -> u16 {
        s.chars()
            .map(|c| u16::try_from(c.width().unwrap_or(0)).unwrap())
            .sum()
    };
    let mut rows: Vec<String> = vec![String::new()];
    let mut col: u16 = 0;
    let mut positions: Vec<Option<(u64, u16)>> = vec![None; rendered.len()];
    let bytes = rendered.as_bytes();
    let mut i = 0;
    while i < rendered.len() {
        if bytes[i] == b' ' {
            let word_start = i + 1;
            let word_end = rendered[word_start..]
                .find(' ')
                .map_or(rendered.len(), |k| word_start + k);
            let ww = word_width(&rendered[word_start..word_end]);
            if col > 0 && col + 1 + ww <= width {
                positions[i] = Some((u64::try_from(rows.len()).unwrap() - 1, col));
                rows.last_mut().unwrap().push(' ');
                col += 1;
            } else {
                rows.push(String::new());
                col = 0;
            }
            i += 1;
        } else {
            let end = rendered[i..].find(' ').map_or(rendered.len(), |k| i + k);
            let word = &rendered[i..end];
            if word_width(word) > width && col > 0 {
                rows.push(String::new());
                col = 0;
            }
            for (rel, c) in word.char_indices() {
                let w = u16::try_from(c.width().unwrap_or(0)).unwrap();
                if col + w > width {
                    rows.push(String::new());
                    col = 0;
                }
                for k in 0..c.len_utf8() {
                    positions[i + rel + k] = Some((u64::try_from(rows.len()).unwrap() - 1, col));
                }
                rows.last_mut().unwrap().push(c);
                col += w;
            }
            i = end;
        }
    }
    Wrapped {
        rows: rows
            .into_iter()
            .enumerate()
            .map(|(r, text)| (AbsRow(u64::try_from(r).unwrap()), text))
            .collect(),
        positions,
    }
}

fn expected_cells(
    wrapped: &Wrapped,
    rendered: &str,
    range: Range<usize>,
    facet: &Facet,
    weight: f32,
) -> Vec<CellSpan> {
    let mut runs: Vec<(u64, u16, u16)> = Vec::new();
    for (rel, c) in rendered[range.clone()].char_indices() {
        let Some((row, col)) = wrapped.positions[range.start + rel] else {
            continue;
        };
        let w = u16::try_from(c.width().unwrap_or(0)).unwrap();
        if w == 0 {
            continue;
        }
        let end = col + w;
        match runs.last_mut() {
            Some((run_row, _, run_end)) if *run_row == row && col <= *run_end => {
                *run_end = (*run_end).max(end);
            }
            _ => runs.push((row, col, end)),
        }
    }
    runs.into_iter()
        .map(|(row, start, end)| CellSpan {
            row: AbsRow(row),
            cols: start..end,
            facet: facet.clone(),
            weight,
        })
        .collect()
}

#[test]
fn wrap_matrix_maps_spans_at_all_widths() {
    let path = "/opt/deeply/nested/pipeline/segment-a/segment-b/segment-c/segment-d/segment-e/segment-f/segment-g/segment-h/segment-i/segment-j/tail.log";
    let clean = format!(
        "Deploy check: we will **erase the production 数据库 now** and then run `--dry-run` 🚀 verification on {path} before finishing the rollout of everything."
    );
    let (rendered, _) = normalize_markdown(&clean);
    let cases: Vec<(&str, &str, Facet, f32)> = vec![
        (
            "**erase the production 数据库 now**",
            "erase the production 数据库 now",
            Facet::Risk,
            0.9,
        ),
        ("`--dry-run`", "--dry-run", Facet::Command, 0.8),
        ("🚀 verification", "🚀 verification", Facet::Novelty, 0.6),
        (path, path, Facet::Action, 0.75),
    ];
    for width in [40u16, 80, 120] {
        let wrapped = wrap(&rendered, width);
        assert!(
            wrapped.rows.len() > 1,
            "width {width}: paragraph must wrap"
        );
        for (marked, inner, facet, weight) in &cases {
            let span = SpanScore::new(
                find_range(&clean, marked),
                facet.clone(),
                *weight,
                SourceId::Anthropic,
            );
            let got = map_spans(&clean, &[span], &wrapped.rows, &[]);
            let expected =
                expected_cells(&wrapped, &rendered, find_range(&rendered, inner), facet, *weight);
            assert!(!expected.is_empty(), "width {width} {inner:?}: expectation empty");
            assert_eq!(got, expected, "width {width} span {inner:?}");
        }
    }
}

#[test]
fn hard_split_path_spans_multiple_rows() {
    let path = "/opt/deeply/nested/pipeline/segment-a/segment-b/segment-c/segment-d/tail.log";
    let clean = format!("reading {path} now");
    let (rendered, _) = normalize_markdown(&clean);
    let wrapped = wrap(&rendered, 40);
    let span = SpanScore::new(find_range(&clean, path), Facet::Action, 0.7, SourceId::Anthropic);
    let got = map_spans(&clean, &[span], &wrapped.rows, &[]);
    assert!(got.len() >= 2, "hard-split path must cover multiple rows: {got:?}");
    let expected = expected_cells(&wrapped, &rendered, find_range(&rendered, path), &Facet::Action, 0.7);
    assert_eq!(got, expected);
}

#[test]
fn gutter_prefixes_strip_and_offset_cols() {
    let clean = "I will run the tests and check coverage of the mapper";
    let window = vec![
        (AbsRow(40), "⏺ I will run the tests".to_owned()),
        (AbsRow(41), "  and check coverage".to_owned()),
        (AbsRow(42), "│ of the mapper".to_owned()),
    ];
    let gutters = ["⏺ ", "│ ", "  "];
    let spans = [
        SpanScore::new(find_range(clean, "run the tests"), Facet::Command, 0.8, SourceId::Anthropic),
        SpanScore::new(find_range(clean, "check coverage"), Facet::Action, 0.6, SourceId::Anthropic),
        SpanScore::new(find_range(clean, "mapper"), Facet::Novelty, 0.5, SourceId::Anthropic),
    ];
    let got = map_spans(clean, &spans, &window, &gutters);
    let w = |s: &str| u16::try_from(UnicodeWidthStr::width(s)).unwrap();
    let expected = vec![
        CellSpan {
            row: AbsRow(40),
            cols: w("⏺ I will ")..w("⏺ I will run the tests"),
            facet: Facet::Command,
            weight: 0.8,
        },
        CellSpan {
            row: AbsRow(41),
            cols: w("  and ")..w("  and check coverage"),
            facet: Facet::Action,
            weight: 0.6,
        },
        CellSpan {
            row: AbsRow(42),
            cols: w("│ of the ")..w("│ of the mapper"),
            facet: Facet::Novelty,
            weight: 0.5,
        },
    ];
    assert_eq!(got, expected);
}

#[test]
fn ambiguity_and_absence_drop_unique_fallback_maps() {
    let window = vec![
        (AbsRow(7), "retry the request".to_owned()),
        (AbsRow(8), "something unrelated".to_owned()),
        (AbsRow(9), "retry the request".to_owned()),
        (AbsRow(10), "apply the unique patch here".to_owned()),
    ];
    let clean =
        "you should retry the request and later apply the unique patch quickly, then vanish entirely";
    let spans = [
        SpanScore::new(find_range(clean, "retry the request"), Facet::Action, 0.7, SourceId::Anthropic),
        SpanScore::new(find_range(clean, "apply the unique patch"), Facet::Command, 0.8, SourceId::Anthropic),
        SpanScore::new(find_range(clean, "vanish entirely"), Facet::Risk, 0.9, SourceId::Anthropic),
    ];
    let got = map_spans(clean, &spans, &window, &[]);
    let expected = vec![CellSpan {
        row: AbsRow(10),
        cols: 0..u16::try_from("apply the unique patch".len()).unwrap(),
        facet: Facet::Command,
        weight: 0.8,
    }];
    assert_eq!(got, expected);
}

#[test]
fn fallback_rejects_in_word_substring() {
    let window = vec![(AbsRow(3), "we will concatenate the buffers".to_owned())];
    let clean = "unrelated preamble so the whole message never anchors, but cat appears";
    let spans = [SpanScore::new(find_range(clean, "cat"), Facet::Risk, 0.9, SourceId::Anthropic)];
    let got = map_spans(clean, &spans, &window, &[]);
    assert!(got.is_empty(), "span 'cat' must not style the interior of 'concatenate': {got:?}");
}

#[test]
fn fallback_allows_wrap_seam_boundary() {
    let window = vec![
        (AbsRow(3), "please run the tests".to_owned()),
        (AbsRow(4), "and report back".to_owned()),
    ];
    let clean = "earlier unrelated text that blocks anchoring, then run the tests appears";
    let spans = [SpanScore::new(find_range(clean, "run the tests"), Facet::Command, 0.8, SourceId::Anthropic)];
    let got = map_spans(clean, &spans, &window, &[]);
    assert_eq!(got.len(), 1, "wrap-seam boundary must still map: {got:?}");
    assert_eq!(got[0].row, AbsRow(3));
}

#[test]
fn anchor_requires_unique_match() {
    assert_eq!(anchor("ab", "xxabyy"), Some(2));
    assert_eq!(anchor("ab", "abab"), None);
    assert_eq!(anchor("zz", "abab"), None);
    assert_eq!(anchor("", "abab"), None);
}

fn feed(grid: &mut ShadowGrid, fin: &mut Finalizer, log: &mut Vec<u8>, bytes: &[u8]) {
    log.extend_from_slice(bytes);
    grid.process(bytes);
    let events = grid.telemetry_mut().take_events();
    fin.ingest(events);
}

fn burst_end(grid: &mut ShadowGrid, fin: &mut Finalizer) {
    fin.note_burst_end();
    grid.telemetry_mut().end_burst();
}

fn session(lines: &[String]) -> (ShadowGrid, Finalizer, Vec<u8>) {
    let mut grid = ShadowGrid::new(24, 80);
    let mut fin = Finalizer::new(24);
    let mut log = Vec::new();
    for line in lines {
        feed(&mut grid, &mut fin, &mut log, format!("{line}\r\n").as_bytes());
    }
    feed(&mut grid, &mut fin, &mut log, "\x1b[24;1H╰ esc to interrupt".as_bytes());
    burst_end(&mut grid, &mut fin);
    (grid, fin, log)
}

fn plain_lines() -> Vec<String> {
    (0..28u32)
        .map(|i| format!("content line {i:02} with some interesting words"))
        .collect()
}

fn cup_targets(batch: &[u8]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < batch.len() {
        if batch[i] == 0x1b && batch[i + 1] == b'[' {
            let mut j = i + 2;
            while j < batch.len() && (batch[j].is_ascii_digit() || batch[j] == b';') {
                j += 1;
            }
            if j < batch.len() && batch[j] == b'H' {
                let body = std::str::from_utf8(&batch[i + 2..j]).unwrap();
                let (r, c) = body.split_once(';').unwrap_or((body, "1"));
                out.push((r.parse().unwrap(), c.parse().unwrap()));
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn cell_attrs(cell: &vt100::Cell) -> (vt100::Color, vt100::Color, bool, bool, bool, bool, bool) {
    (
        cell.fgcolor(),
        cell.bgcolor(),
        cell.bold(),
        cell.dim(),
        cell.italic(),
        cell.underline(),
        cell.inverse(),
    )
}

#[test]
fn repaint_batch_targets_queued_rows_and_restores_cursor() {
    let (mut grid, fin, log) = session(&plain_lines());
    let base = grid.scroll_base();
    assert_eq!(base, 5);
    let spans = vec![
        CellSpan {
            row: AbsRow(10),
            cols: 8..24,
            facet: Facet::Risk,
            weight: 0.95,
        },
        CellSpan {
            row: AbsRow(15),
            cols: 0..12,
            facet: Facet::Action,
            weight: 0.5,
        },
    ];
    let mut engine = RepaintEngine::new(0.12);
    engine.queue(&mut grid, spans.clone());
    let caps = Caps::truecolor_undercurl();
    let theme = Theme::default();
    let batch = engine
        .build_batch(&mut grid, &fin, &caps, &theme)
        .expect("eligible rows produce a batch");
    assert!(batch.starts_with(b"\x1b[?2026h\x1b[?25l"));
    assert!(batch.ends_with(b"\x1b[?25h\x1b[?2026l"));
    let cups = cup_targets(&batch);
    let (cur_row, cur_col) = grid.cursor();
    assert_eq!(
        cups,
        vec![(6, 1), (11, 1), (cur_row + 1, cur_col + 1)],
        "CUP targets must be exactly the two queued visible rows plus the cursor restore"
    );

    let mut reference = vt100::Parser::new(24, 80, 10_000);
    reference.process(&log);
    let before = reference.screen().clone();
    reference.process(&batch);
    let after = reference.screen();
    assert_eq!(after.contents(), before.contents(), "repaint must not change text");
    assert_eq!(after.cursor_position(), before.cursor_position());
    for row in 0..24u16 {
        for col in 0..80u16 {
            let b = before.cell(row, col).unwrap();
            let a = after.cell(row, col).unwrap();
            if row == 5 && (8..24).contains(&col) {
                assert_eq!(a.fgcolor(), vt100::Color::Rgb(0xD5, 0x5E, 0x00), "risk hue at ({row},{col})");
                assert!(a.bold(), "risk weight 0.95 bolds at ({row},{col})");
            } else if row == 10 && (0..12).contains(&col) {
                assert_eq!(a.fgcolor(), vt100::Color::Rgb(0x00, 0x72, 0xB2), "action hue at ({row},{col})");
                assert!(!a.bold(), "action weight 0.5 must not bold at ({row},{col})");
            } else {
                assert_eq!(
                    cell_attrs(a),
                    cell_attrs(b),
                    "attrs outside overlay changed at ({row},{col})"
                );
            }
        }
    }

    engine.queue(&mut grid, spans);
    let second = engine
        .build_batch(&mut grid, &fin, &caps, &theme)
        .expect("re-queue against unchanged state rebuilds");
    assert_eq!(batch, second, "same state must produce a byte-identical batch");
}

#[test]
fn repaint_without_sync_update_omits_2026_and_empty_queue_is_none() {
    let (mut grid, fin, _log) = session(&plain_lines());
    let caps = Caps {
        styling_enabled: true,
        depth: ColorDepth::TrueColor,
        undercurl: true,
        sync_update: false,
    };
    let theme = Theme::default();
    let mut engine = RepaintEngine::new(0.12);
    assert!(engine.build_batch(&mut grid, &fin, &caps, &theme).is_none());
    engine.queue(
        &mut grid,
        vec![CellSpan {
            row: AbsRow(10),
            cols: 0..5,
            facet: Facet::Command,
            weight: 0.8,
        }],
    );
    let batch = engine.build_batch(&mut grid, &fin, &caps, &theme).expect("batch");
    assert!(!batch.windows(6).any(|w| w == b"\x1b[?2026"), "no sync-update wrap without the cap");
    assert!(batch.starts_with(b"\x1b[?25l"));
    assert!(batch.ends_with(b"\x1b[?25h"));
}

#[test]
fn repaint_glyph_swaps_two_space_pad_only() {
    let mut lines = plain_lines();
    lines[10] = "  padded step ready for glyph swap".to_owned();
    let (mut grid, fin, log) = session(&lines);
    let mut engine = RepaintEngine::new(0.12);
    engine.queue(
        &mut grid,
        vec![
            CellSpan {
                row: AbsRow(10),
                cols: 2..8,
                facet: Facet::Risk,
                weight: 0.9,
            },
            CellSpan {
                row: AbsRow(15),
                cols: 0..7,
                facet: Facet::Command,
                weight: 0.8,
            },
        ],
    );
    let caps = Caps::truecolor_undercurl();
    let theme = Theme::default();
    let batch = engine.build_batch(&mut grid, &fin, &caps, &theme).expect("batch");
    let mut reference = vt100::Parser::new(24, 80, 10_000);
    reference.process(&log);
    let before = reference.screen().clone();
    reference.process(&batch);
    let after = reference.screen();
    assert_eq!(after.cell(5, 0).unwrap().contents(), "⚠");
    assert_eq!(after.cell(5, 1).unwrap().contents(), " ");
    for col in 2..80u16 {
        assert_eq!(
            after.cell(5, col).unwrap().contents(),
            before.cell(5, col).unwrap().contents(),
            "padded row text beyond the glyph changed at col {col}"
        );
    }
    for row in 0..24u16 {
        if row == 5 {
            continue;
        }
        for col in 0..80u16 {
            assert_eq!(
                after.cell(row, col).unwrap().contents(),
                before.cell(row, col).unwrap().contents(),
                "non-glyph row text changed at ({row},{col})"
            );
        }
    }
}

#[test]
fn repaint_skips_rewritten_and_scrolled_rows() {
    let (mut grid, mut fin, mut log) = session(&plain_lines());
    let mut engine = RepaintEngine::new(0.12);
    engine.queue(
        &mut grid,
        vec![CellSpan {
            row: AbsRow(10),
            cols: 0..10,
            facet: Facet::Risk,
            weight: 0.9,
        }],
    );
    feed(&mut grid, &mut fin, &mut log, b"\x1b[6;1Hrewritten content that differs");
    feed(&mut grid, &mut fin, &mut log, "\x1b[24;1H╰ esc to interrupt".as_bytes());
    burst_end(&mut grid, &mut fin);
    feed(&mut grid, &mut fin, &mut log, b"\x1b[24;1H");
    burst_end(&mut grid, &mut fin);
    feed(&mut grid, &mut fin, &mut log, b"\x1b[24;1H");
    burst_end(&mut grid, &mut fin);
    let caps = Caps::truecolor_undercurl();
    let theme = Theme::default();
    assert!(
        engine.build_batch(&mut grid, &fin, &caps, &theme).is_none(),
        "rewritten row must emit nothing"
    );

    let (mut grid, mut fin, mut log) = session(&plain_lines());
    let mut engine = RepaintEngine::new(0.12);
    engine.queue(
        &mut grid,
        vec![CellSpan {
            row: AbsRow(20),
            cols: 0..10,
            facet: Facet::Action,
            weight: 0.7,
        }],
    );
    for i in 0..30u32 {
        feed(&mut grid, &mut fin, &mut log, format!("\r\nlater line {i:02}").as_bytes());
    }
    burst_end(&mut grid, &mut fin);
    assert!(grid.visible_row_of(AbsRow(20)).is_none(), "row 20 must have scrolled out");
    assert!(
        engine.build_batch(&mut grid, &fin, &caps, &theme).is_none(),
        "scrolled-out row must emit nothing"
    );
}
