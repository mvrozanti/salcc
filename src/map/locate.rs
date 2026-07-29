//! Public mapping entry: clean-text SpanScores -> grid CellSpans.
//!
//! Per span: project the clean-text byte range through the markdown source
//! map (partition point over the monotonic map), take the anchored grid
//! range when the whole message anchored, otherwise fall back to locating
//! the span's own normalized text in the grid — a UNIQUE occurrence maps,
//! zero or multiple candidates DROP the span (invariant 4, debug-logged).
//! Provenance is total, so a located span always maps completely: cell runs
//! merge per row into contiguous `CellSpan`s (a wrapped span yields one per
//! row), coverage extends to the full cell width of the final character
//! (CJK, emoji). A span never partially maps — it either produces runs for
//! every character it covers or nothing.

use std::ops::Range;

use memchr::memmem;
use unicode_width::UnicodeWidthChar;

use crate::map::align::anchor;
use crate::map::normalize::{CellRef, normalize_grid, normalize_markdown};
use crate::types::{AbsRow, Facet, SpanScore};

#[derive(Debug, Clone, PartialEq)]
pub struct CellSpan {
    pub row: AbsRow,
    pub cols: Range<u16>,
    pub facet: Facet,
    pub weight: f32,
}

pub fn map_spans(
    clean: &str,
    spans: &[SpanScore],
    window: &[(AbsRow, String)],
    gutters: &[&str],
) -> Vec<CellSpan> {
    let (clean_norm, md_map) = normalize_markdown(clean);
    let (grid_norm, grid_map) = normalize_grid(window, gutters);
    if clean_norm.is_empty() || grid_norm.is_empty() {
        return Vec::new();
    }
    let anchored = anchor(&clean_norm, &grid_norm);
    let mut out = Vec::new();
    for span in spans {
        match locate_span(span, &clean_norm, &md_map, &grid_norm, anchored) {
            Some(range) => out.extend(cell_runs(&grid_norm, &grid_map, range, span)),
            None => tracing::debug!(
                range = ?span.range,
                facet = ?span.facet,
                anchored = anchored.is_some(),
                "span dropped: no unambiguous grid match"
            ),
        }
    }
    out.sort_by_key(|s| (s.row, s.cols.start));
    out
}

fn locate_span(
    span: &SpanScore,
    clean_norm: &str,
    md_map: &[usize],
    grid_norm: &str,
    anchored: Option<usize>,
) -> Option<Range<usize>> {
    let norm = project_range(md_map, &span.range)?;
    if !clean_norm.is_char_boundary(norm.start) || !clean_norm.is_char_boundary(norm.end) {
        return None;
    }
    if let Some(base) = anchored {
        return Some(base + norm.start..base + norm.end);
    }
    unique_occurrence(&clean_norm[norm], grid_norm)
}

fn project_range(md_map: &[usize], range: &Range<usize>) -> Option<Range<usize>> {
    let start = md_map.partition_point(|&origin| origin < range.start);
    let end = md_map.partition_point(|&origin| origin < range.end);
    (start < end).then_some(start..end)
}

fn unique_occurrence(needle: &str, hay: &str) -> Option<Range<usize>> {
    if needle.is_empty() {
        return None;
    }
    let mut hits = memmem::find_iter(hay.as_bytes(), needle.as_bytes());
    let first = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    Some(first..first + needle.len())
}

fn cell_runs(
    grid_norm: &str,
    grid_map: &[CellRef],
    range: Range<usize>,
    span: &SpanScore,
) -> Vec<CellSpan> {
    let mut runs: Vec<(AbsRow, u16, u16)> = Vec::new();
    for (rel, c) in grid_norm[range.clone()].char_indices() {
        let cell = grid_map[range.start + rel];
        let width = u16::try_from(c.width().unwrap_or(0)).unwrap_or(0);
        if width == 0 {
            continue;
        }
        let end = cell.col.saturating_add(width);
        match runs.last_mut() {
            Some((row, _, run_end)) if *row == cell.row && cell.col <= *run_end => {
                *run_end = (*run_end).max(end);
            }
            _ => runs.push((cell.row, cell.col, end)),
        }
    }
    runs.into_iter()
        .map(|(row, start, end)| CellSpan {
            row,
            cols: start..end,
            facet: span.facet.clone(),
            weight: span.weight,
        })
        .collect()
}
