//! Two render-lite normalizers with per-byte provenance, feeding the
//! span -> cell mapper.
//!
//! `normalize_markdown` projects scored clean text (transcript markdown)
//! onto what the terminal renders: fence lines vanish (fence interior is
//! verbatim — code keeps its `*`), `**`/`*`/`__`/backtick markers drop,
//! `#` header and `>` quote prefixes strip, `[text](url)` keeps only text,
//! whitespace runs (including newlines) collapse to one space, ends trim.
//! The source map is byte-parallel: `map[i]` is the byte offset in the
//! original clean text that produced normalized byte `i`, monotonically
//! non-decreasing so span ranges project via partition point.
//!
//! `normalize_grid` concatenates window row texts with per-byte `CellRef`
//! provenance in CELL columns (unicode-width: CJK = 2 cells, provenance
//! points at the first cell). Gutter prefixes strip at row start when
//! present. Whitespace collapses to one space anchored at the first
//! whitespace cell; a pure row join anchors its space at the next row's
//! first content cell. A row join is seamless (no space — the child
//! hard-wrapped mid-word) only when the previous row's raw cell width
//! equals the window's maximum row width, it ends in non-whitespace, and
//! the next row starts with non-whitespace; a soft-wrapped row that
//! coincidentally fills the full width mis-joins and the affected span
//! drops downstream — fail-safe, never wrong styling.

use unicode_width::UnicodeWidthChar;

use crate::types::AbsRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRef {
    pub row: AbsRow,
    pub col: u16,
}

pub fn normalize_markdown(clean: &str) -> (String, Vec<usize>) {
    let mut out = String::new();
    let mut map: Vec<usize> = Vec::new();
    let mut pending: Option<usize> = None;
    let mut in_fence = false;
    let mut offset = 0;
    for raw_line in clean.split_inclusive('\n') {
        let line_offset = offset;
        offset += raw_line.len();
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        if line.trim().starts_with("```") {
            in_fence = !in_fence;
            pending.get_or_insert(line_offset);
            continue;
        }
        let from = if in_fence { 0 } else { prefix_len(line) };
        scan_inline(
            line,
            line_offset,
            from,
            in_fence,
            &mut out,
            &mut map,
            &mut pending,
        );
        pending.get_or_insert(line_offset + line.len());
    }
    (out, map)
}

pub fn normalize_grid(rows: &[(AbsRow, String)], gutters: &[&str]) -> (String, Vec<CellRef>) {
    let mut out = String::new();
    let mut map: Vec<CellRef> = Vec::new();
    let mut pending: Option<Option<CellRef>> = None;
    let widths: Vec<u16> = rows.iter().map(|(_, text)| text_cell_width(text)).collect();
    let max_width = widths.iter().copied().max().unwrap_or(0);
    for (idx, (row, text)) in rows.iter().enumerate() {
        let (start_byte, start_col) = gutter_prefix(text, gutters);
        let mut col = start_col;
        for c in text[start_byte..].chars() {
            let width = char_cell_width(c);
            if c.is_whitespace() {
                if pending.is_none() {
                    pending = Some(Some(CellRef { row: *row, col }));
                }
                col = col.saturating_add(width.max(1));
                continue;
            }
            let cell = CellRef { row: *row, col };
            if let Some(gap) = pending.take()
                && !out.is_empty()
            {
                out.push(' ');
                map.push(gap.unwrap_or(cell));
            }
            out.push(c);
            for _ in 0..c.len_utf8() {
                map.push(cell);
            }
            col = col.saturating_add(width);
        }
        if idx + 1 < rows.len() {
            let seamless = pending.is_none()
                && widths[idx] > 0
                && widths[idx] == max_width
                && starts_nonspace(&rows[idx + 1].1, gutters);
            if !seamless && pending.is_none() {
                pending = Some(None);
            }
        }
    }
    (out, map)
}

fn prefix_len(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut i = 0;
    loop {
        let mut j = i;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'>' {
            i = j + 1;
            if i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
        } else {
            break;
        }
    }
    let mut h = i;
    while h < bytes.len() && (bytes[h] == b' ' || bytes[h] == b'\t') {
        h += 1;
    }
    let mut hashes = 0;
    while h + hashes < bytes.len() && bytes[h + hashes] == b'#' {
        hashes += 1;
    }
    if (1..=6).contains(&hashes)
        && h + hashes < bytes.len()
        && (bytes[h + hashes] == b' ' || bytes[h + hashes] == b'\t')
    {
        h + hashes
    } else {
        i
    }
}

fn scan_inline(
    line: &str,
    line_offset: usize,
    from: usize,
    verbatim: bool,
    out: &mut String,
    map: &mut Vec<usize>,
    pending: &mut Option<usize>,
) {
    let bytes = line.as_bytes();
    let mut j = from;
    let mut link_close: Option<(usize, usize)> = None;
    while j < line.len() {
        if let Some((close, resume)) = link_close
            && j == close
        {
            j = resume;
            link_close = None;
            continue;
        }
        let c = line[j..].chars().next().expect("char at boundary");
        let len = c.len_utf8();
        if c.is_whitespace() {
            pending.get_or_insert(line_offset + j);
            j += len;
            continue;
        }
        if !verbatim {
            match c {
                '*' | '`' => {
                    j += len;
                    continue;
                }
                '_' if bytes.get(j + 1) == Some(&b'_') => {
                    j += 2;
                    continue;
                }
                '[' if link_close.is_none() => {
                    if let Some(bounds) = link_bounds(line, j) {
                        link_close = Some(bounds);
                        j += 1;
                        continue;
                    }
                }
                _ => {}
            }
        }
        emit(c, line_offset + j, out, map, pending);
        j += len;
    }
}

fn link_bounds(line: &str, open: usize) -> Option<(usize, usize)> {
    let close = open + 1 + line[open + 1..].find(']')?;
    let rest = &line[close + 1..];
    if !rest.starts_with('(') {
        return None;
    }
    let paren = close + 2 + rest[1..].find(')')?;
    Some((close, paren + 1))
}

fn emit(c: char, origin: usize, out: &mut String, map: &mut Vec<usize>, pending: &mut Option<usize>) {
    if let Some(ws) = pending.take()
        && !out.is_empty()
    {
        out.push(' ');
        map.push(ws);
    }
    out.push(c);
    for k in 0..c.len_utf8() {
        map.push(origin + k);
    }
}

fn gutter_prefix(text: &str, gutters: &[&str]) -> (usize, u16) {
    for gutter in gutters {
        if !gutter.is_empty() && text.starts_with(gutter) {
            return (gutter.len(), text_cell_width(gutter));
        }
    }
    (0, 0)
}

fn starts_nonspace(text: &str, gutters: &[&str]) -> bool {
    let (start, _) = gutter_prefix(text, gutters);
    text[start..].chars().next().is_some_and(|c| !c.is_whitespace())
}

fn text_cell_width(text: &str) -> u16 {
    text.chars()
        .map(char_cell_width)
        .fold(0u16, u16::saturating_add)
}

fn char_cell_width(c: char) -> u16 {
    u16::try_from(c.width().unwrap_or(0)).unwrap_or(0)
}
