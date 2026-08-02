//! Visible-region repaint engine: the M3 refinement channel that applies
//! late LLM CellSpans to rows already emitted. A row repaints only while
//! visible AND ScrollbackBound AND unchanged since queue time (generation
//! via `Finalizer::is_still_valid` plus a row-text hash); anything else
//! drops silently with a debug log — invariants 3 and 4: never touch the
//! child's active region, never style the wrong text.
//!
//! `queue` captures each row's generation and text hash at queue time;
//! re-queueing a row whose content changed since resets its overlay.
//! Overlapping spans on one row resolve per cell to the highest weight.
//!
//! `build_batch` drains the queue and returns one byte batch or None. The
//! bytes are a pure function of (queued overlays, grid, caps, theme):
//! re-queueing identical spans against unchanged state reproduces a
//! byte-identical batch. Structure: CSI ?2026h when `caps.sync_update`,
//! CSI ?25l, then per eligible row CUP(vis+1,1) + a full attrs-faithful
//! re-emission of the row (vt100 cell attrs as base, facet overlay through
//! the shared styler vocabulary: luminance is ours, hue only where the
//! child left default fg except Risk always, glyph only replacing a 2-space
//! gutter pad, never a width change, no reverse video — that budget belongs
//! to the emission channel) + SGR reset + EL(0) for the blank tail (skipped
//! when content reaches the last column so EL cannot eat the final cell),
//! then child live-SGR restore + CUP to the grid cursor + CSI ?25h (unless
//! the child hid the cursor) + CSI ?2026l. The restoring CUP cannot reproduce
//! a pending-wrap (deferred-autowrap) cursor state, so `build_batch` defers
//! the whole batch when the child cursor sits on the last column.
//!
//! Rate limiting is the caller's job: drive from the pipeline flush tick,
//! child-quiet gated, at most one batch per 100ms (docs/architecture.md).

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

use crate::config::theme::Theme;
use crate::emit::styler::{facet_sgr_params, fg_params, gutter_glyph_slot, vt100_color_param};
use crate::map::locate::CellSpan;
use crate::term::caps::Caps;
use crate::term::finalize::Finalizer;
use crate::term::grid::ShadowGrid;
use crate::types::{AbsRow, Facet, Finality};

#[derive(Debug, Clone)]
struct OverlaySpan {
    cols: Range<u16>,
    facet: Facet,
    weight: f32,
}

#[derive(Debug, Clone)]
struct RowOverlay {
    generation: u64,
    text_hash: u64,
    spans: Vec<OverlaySpan>,
}

#[derive(Debug)]
pub struct RepaintEngine {
    queued: BTreeMap<u64, RowOverlay>,
    density_budget: f32,
}

impl RepaintEngine {
    pub fn new(density_budget: f32) -> Self {
        Self {
            queued: BTreeMap::new(),
            density_budget,
        }
    }

    pub fn pending_rows(&self) -> usize {
        self.queued.len()
    }

    pub fn clear(&mut self) {
        self.queued.clear();
    }

    pub fn queue(&mut self, grid: &mut ShadowGrid, spans: impl IntoIterator<Item = CellSpan>) {
        for span in spans {
            let Some(text) = grid.row_text(span.row) else {
                tracing::debug!(row = span.row.0, "queue skipped: row text unavailable");
                continue;
            };
            let generation = grid.row_generation(span.row);
            let text_hash = hash_text(&text);
            let overlay = self.queued.entry(span.row.0).or_insert_with(|| RowOverlay {
                generation,
                text_hash,
                spans: Vec::new(),
            });
            if overlay.generation != generation || overlay.text_hash != text_hash {
                tracing::debug!(row = span.row.0, "row changed since first queue; overlay reset");
                *overlay = RowOverlay {
                    generation,
                    text_hash,
                    spans: Vec::new(),
                };
            }
            overlay.spans.push(OverlaySpan {
                cols: span.cols,
                facet: span.facet,
                weight: span.weight,
            });
        }
    }

    pub fn build_batch(
        &mut self,
        grid: &mut ShadowGrid,
        fin: &Finalizer,
        caps: &Caps,
        theme: &Theme,
    ) -> Option<Vec<u8>> {
        if self.queued.is_empty() || !caps.styling_enabled {
            return None;
        }
        let (_, cols) = grid.screen().size();
        if grid.cursor().1 + 1 >= cols {
            tracing::debug!("repaint deferred: cursor at last column, pending-wrap state unrestorable");
            return None;
        }
        let queued = std::mem::take(&mut self.queued);
        let mut eligible: Vec<(u16, String, RowOverlay)> = Vec::new();
        for (abs, overlay) in queued {
            let row = AbsRow(abs);
            let Some(vis) = grid.visible_row_of(row) else {
                tracing::debug!(row = abs, "repaint dropped: row no longer visible");
                continue;
            };
            if fin.classify(row) != Finality::ScrollbackBound {
                tracing::debug!(row = abs, "repaint dropped: row not scrollback-bound");
                continue;
            }
            let Some(text) = grid.row_text(row) else {
                tracing::debug!(row = abs, "repaint dropped: row text unavailable");
                continue;
            };
            if !fin.is_still_valid(grid, row, overlay.generation) {
                tracing::debug!(row = abs, "repaint dropped: generation changed since queue");
                continue;
            }
            if hash_text(&text) != overlay.text_hash {
                tracing::debug!(row = abs, "repaint dropped: row text changed since queue");
                continue;
            }
            eligible.push((vis, text, overlay));
        }
        if eligible.is_empty() {
            return None;
        }
        for (_, text, overlay) in &mut eligible {
            enforce_density(&mut overlay.spans, text.chars().count(), self.density_budget);
        }
        let mut batch = Vec::new();
        if caps.sync_update {
            batch.extend_from_slice(b"\x1b[?2026h");
        }
        batch.extend_from_slice(b"\x1b[?25l");
        for (vis, text, overlay) in &eligible {
            emit_row(&mut batch, grid.screen(), *vis, text, overlay, caps, theme);
        }
        batch.extend_from_slice(&grid.telemetry().live_sgr().restore_sequence());
        let (cur_row, cur_col) = grid.cursor();
        batch.extend_from_slice(format!("\x1b[{};{}H", cur_row + 1, cur_col + 1).as_bytes());
        if !grid.screen().hide_cursor() {
            batch.extend_from_slice(b"\x1b[?25h");
        }
        if caps.sync_update {
            batch.extend_from_slice(b"\x1b[?2026l");
        }
        tracing::debug!(rows = eligible.len(), bytes = batch.len(), "repaint batch built");
        Some(batch)
    }
}

fn emit_row(
    out: &mut Vec<u8>,
    screen: &vt100::Screen,
    vis: u16,
    text: &str,
    overlay: &RowOverlay,
    caps: &Caps,
    theme: &Theme,
) {
    out.extend_from_slice(format!("\x1b[{};1H", vis + 1).as_bytes());
    let (_, cols) = screen.size();
    let glyph = glyph_swap(text, overlay, theme);
    let last = (0..cols).rev().find(|col| {
        screen
            .cell(vis, *col)
            .is_some_and(|c| c.has_contents() || c.is_wide_continuation())
    });
    let mut active: Option<String> = None;
    let mut col = 0u16;
    while last.is_some_and(|l| col <= l) {
        let Some(cell) = screen.cell(vis, col) else {
            break;
        };
        if cell.is_wide_continuation() {
            col += 1;
            continue;
        }
        let params = match &glyph {
            Some((_, facet)) if col == 0 => glyph_params(theme, caps, facet),
            _ => cell_params(cell, strongest_span(overlay, col), caps, theme),
        };
        let joined = params.join(";");
        if active.as_deref() != Some(joined.as_str()) {
            out.extend_from_slice(format!("\x1b[{joined}m").as_bytes());
            active = Some(joined);
        }
        match &glyph {
            Some((g, _)) if col == 0 => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(g.encode_utf8(&mut buf).as_bytes());
            }
            _ if cell.has_contents() => out.extend_from_slice(cell.contents().as_bytes()),
            _ => out.push(b' '),
        }
        col += 1;
    }
    out.extend_from_slice(b"\x1b[0m");
    if last.is_none_or(|l| l + 1 < cols) {
        out.extend_from_slice(b"\x1b[K");
    }
}

fn glyph_swap<'a>(text: &str, overlay: &'a RowOverlay, theme: &Theme) -> Option<(char, &'a Facet)> {
    if !gutter_glyph_slot(text) {
        return None;
    }
    let dominant = overlay
        .spans
        .iter()
        .filter(|s| s.facet.is_emphasis())
        .max_by(|a, b| a.weight.total_cmp(&b.weight))?;
    let glyph = theme.for_facet(&dominant.facet)?.gutter_glyph()?;
    Some((glyph, &dominant.facet))
}

fn glyph_params(theme: &Theme, caps: &Caps, facet: &Facet) -> Vec<String> {
    let mut params = vec!["0".to_owned(), "1".to_owned()];
    if let Some(color) = theme.for_facet(facet).and_then(|t| t.color) {
        params.push(fg_params(caps, color));
    }
    params
}

fn cell_params(
    cell: &vt100::Cell,
    span: Option<&OverlaySpan>,
    caps: &Caps,
    theme: &Theme,
) -> Vec<String> {
    let mut params = vec!["0".to_owned()];
    for (on, code) in [
        (cell.bold(), "1"),
        (cell.dim(), "2"),
        (cell.italic(), "3"),
        (cell.underline(), "4"),
        (cell.inverse(), "7"),
    ] {
        if on {
            params.push(code.to_owned());
        }
    }
    params.extend(vt100_color_param(cell.fgcolor(), false));
    params.extend(vt100_color_param(cell.bgcolor(), true));
    if let Some(span) = span {
        params.extend(facet_sgr_params(
            theme,
            caps,
            &span.facet,
            span.weight,
            cell.fgcolor() == vt100::Color::Default,
            false,
        ));
    }
    params
}

fn enforce_density(spans: &mut Vec<OverlaySpan>, visible_chars: usize, budget_frac: f32) {
    let budget = budget_frac * visible_chars.max(1) as f32;
    loop {
        let emphasis: Vec<usize> = spans
            .iter()
            .enumerate()
            .filter(|(_, s)| s.facet.is_emphasis())
            .map(|(i, _)| i)
            .collect();
        let emphasized: u32 = emphasis
            .iter()
            .map(|&i| u32::from(spans[i].cols.end.saturating_sub(spans[i].cols.start)))
            .sum();
        if emphasized as f32 <= budget || emphasis.len() <= 1 {
            return;
        }
        let victim = emphasis
            .iter()
            .copied()
            .filter(|&i| spans[i].facet != Facet::Risk)
            .min_by(|&a, &b| spans[a].weight.total_cmp(&spans[b].weight));
        match victim {
            Some(i) => {
                spans.remove(i);
            }
            None => return,
        }
    }
}

fn strongest_span(overlay: &RowOverlay, col: u16) -> Option<&OverlaySpan> {
    overlay
        .spans
        .iter()
        .filter(|s| s.cols.contains(&col))
        .max_by(|a, b| a.weight.total_cmp(&b.weight))
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}
