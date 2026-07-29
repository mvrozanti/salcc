//! StylingTransform: the emission-channel orchestrator plugged into the
//! io_loop seam. Per chunk: segment into runs; CTRL runs flush the hold and
//! pass through untouched; TEXT runs feed the shadow grid piece-by-piece
//! (split at newlines) so each completed line captures the AbsRow and row
//! generation it landed on, then wait in the hold for a finality verdict.
//! ScrollbackBound fronts drain eagerly; everything else resolves at its
//! deadline (tick-driven `flush`), on CTRL boundaries, on overflow, on
//! resize (a layout disruption: the hold flushes under the old geometry
//! before grid and finalizer resize), or at stream end — ScrollbackBound +
//! generation-still-valid emits styled, any other verdict emits the
//! original bytes untouched. Quiet gaps (>= 100ms
//! without child output) end the burst for the finalizer. Alt-screen output
//! and disabled caps pass through bit-perfect. Reverse video is budgeted to
//! one span per flush batch. Replay drives the same object offline via
//! `note_virtual_quiet` + `finish`, where wall-clock deadlines simply never
//! fire and verdicts settle at end of cast.
//!
//! Async scoring side channel: `spawn_side_channel` runs a dedicated thread
//! hosting a current-thread tokio runtime that discovers the Claude Code
//! session transcript (watch-for-new with a 30s timeout, falling back to the
//! most recent existing session), tails it, scores each new text block
//! through the injected `Scorer` behind the dedupe cache, and delivers
//! `ScoredBlock`s over a std mpsc channel. The transform drains that channel
//! non-blockingly (try_recv) at the top of `transform`/`flush` — it never
//! awaits. Each scored block is consumed twice: (1) spans are mapped onto
//! the current grid window and queued on the RepaintEngine, whose batches
//! are appended from the flush tick only after the hold has fully drained,
//! the child has been quiet >= 100ms, and >= 100ms passed since the last
//! batch; (2) the block is kept (most recent 8) so lines styled later at
//! emission merge its spans: a held line whose normalized clean text occurs
//! exactly once across the kept blocks gets the overlapping spans projected
//! onto it through the shared normalize_markdown source maps — any
//! ambiguity skips the merge (fail-safe). Scoring errors drop the block; a
//! dead channel degrades silently to heuristics-only. Heuristic and merged
//! LLM spans meet in score::cascade::merge at emission: same-facet overlaps
//! resolve to max weight and zero-weight LLM spans veto heuristic spans
//! where the VetoPolicy allows (never Risk). Cache hits append a
//! zero-token, zero-usd metrics record so the stats dashboard can report
//! cache-hit rate; real API calls log themselves inside the scorer. Scored
//! blocks also feed ConvoContext.recent_block_summaries (first line, most
//! recent 4) so later scoring calls carry cheap session context. A SIGHUP
//! reload flag (signals::register_reload_flag) is polled at the flush tick:
//! when raised, Config::load() re-derives the emission-side knobs (theme,
//! density, hold_ms, facet toggles, dimensions, veto policy) in place —
//! the async scorer keeps its spawn-time config.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

use memchr::memmem;

use crate::config::{Config, VetoPolicy};
use crate::config::theme::Theme;
use crate::emit::hold::{HOLD_CAP, HeldLine, Hold};
use crate::emit::segment::{RunKind, Segmenter};
use crate::emit::styler::Styler;
use crate::map::locate::map_spans;
use crate::map::normalize::normalize_markdown;
use crate::proxy::io_loop::ChunkTransform;
use crate::score::anthropic::block_id;
use crate::score::cache::ScoreCache;
use crate::score::cascade;
use crate::score::heuristic::HeuristicScorer;
use crate::stats::metrics::{MetricsLog, Record};
use crate::tail::discover;
use crate::tail::reader::TailReader;
use crate::term::caps::Caps;
use crate::term::finalize::Finalizer;
use crate::term::grid::ShadowGrid;
use crate::term::repaint::RepaintEngine;
use crate::term::telemetry::SgrState;
use crate::types::{AbsRow, ConvoContext, Finality, ScoreRequest, Scorer, SpanScore};

const QUIET_GAP: Duration = Duration::from_millis(100);
const REPAINT_MIN_INTERVAL: Duration = Duration::from_millis(100);
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(30);
const RECENT_SCORED_CAP: usize = 8;
const MERGE_MIN_NORM_LEN: usize = 3;
const CTX_SUMMARY_CAP: usize = 4;
const CTX_SUMMARY_CHARS: usize = 96;
pub const CLAUDE_GUTTERS: [&str; 3] = ["⏺ ", "│ ", "  "];

pub struct ScoredBlock {
    pub message_id: String,
    pub block_index: usize,
    pub clean: String,
    pub spans: Arc<Vec<SpanScore>>,
}

pub struct CacheHitLog {
    pub metrics: MetricsLog,
    pub model: String,
}

pub enum SessionSource {
    Discover { cwd: PathBuf, spawned_at: SystemTime },
    Attached(TailReader),
}

pub fn spawn_side_channel(
    source: SessionSource,
    scorer: Box<dyn Scorer>,
    cache_log: Option<CacheHitLog>,
) -> Receiver<ScoredBlock> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("salcc-score".into())
        .spawn(move || side_channel_thread(source, scorer, cache_log, tx));
    if let Err(err) = spawned {
        tracing::warn!(%err, "scoring thread failed to spawn; styling continues heuristics-only");
    }
    rx
}

fn side_channel_thread(
    source: SessionSource,
    scorer: Box<dyn Scorer>,
    cache_log: Option<CacheHitLog>,
    tx: Sender<ScoredBlock>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::warn!(%err, "scoring runtime failed to build; styling continues heuristics-only");
            return;
        }
    };
    runtime.block_on(score_new_blocks(source, scorer, cache_log, tx));
    tracing::debug!("scoring side channel thread exited");
}

async fn score_new_blocks(
    source: SessionSource,
    scorer: Box<dyn Scorer>,
    cache_log: Option<CacheHitLog>,
    tx: Sender<ScoredBlock>,
) {
    let Some(reader) = resolve_reader(source).await else {
        return;
    };
    let session_id = reader
        .session_path()
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut ctx = ConvoContext {
        session_id,
        ..ConvoContext::default()
    };
    let cache = ScoreCache::new();
    let mut blocks_per_message: HashMap<String, usize> = HashMap::new();
    let mut blocks = reader.spawn();
    while let Some(block) = blocks.recv().await {
        let seen = blocks_per_message.entry(block.message_id.clone()).or_insert(0);
        let block_index = *seen;
        *seen += 1;
        let scored = cache
            .get_or_score(&block.message_id, block_index, &block.block_text, || {
                scorer.score(ScoreRequest {
                    text: &block.block_text,
                    ctx: &ctx,
                })
            })
            .await;
        match scored {
            Ok((spans, cache_hit)) => {
                tracing::debug!(
                    message_id = %block.message_id,
                    block_index,
                    cache_hit,
                    spans = spans.len(),
                    "block scored"
                );
                if cache_hit && let Some(cache_log) = &cache_log {
                    cache_log.metrics.append(&Record {
                        ts: Record::now_ms(),
                        session: ctx.session_id.clone(),
                        block_id: block_id(&block.block_text),
                        model: cache_log.model.clone(),
                        input_tokens: 0,
                        output_tokens: 0,
                        usd: 0.0,
                        spans_returned: spans.len(),
                        cache_hit: true,
                    });
                }
                note_block_summary(&mut ctx, &block.block_text);
                let delivery = tx.send(ScoredBlock {
                    message_id: block.message_id,
                    block_index,
                    clean: block.block_text,
                    spans,
                });
                if delivery.is_err() {
                    tracing::debug!("score receiver dropped; side channel exiting");
                    return;
                }
            }
            Err(err) => tracing::warn!(
                %err,
                message_id = %block.message_id,
                block_index,
                "block scoring failed; block dropped without retry"
            ),
        }
    }
}

fn note_block_summary(ctx: &mut ConvoContext, block_text: &str) {
    let first_line = block_text.lines().find(|line| !line.trim().is_empty());
    let Some(first_line) = first_line else {
        return;
    };
    let summary: String = first_line.trim().chars().take(CTX_SUMMARY_CHARS).collect();
    ctx.recent_block_summaries.push(summary);
    if ctx.recent_block_summaries.len() > CTX_SUMMARY_CAP {
        ctx.recent_block_summaries.remove(0);
    }
}

async fn resolve_reader(source: SessionSource) -> Option<TailReader> {
    let (cwd, spawned_at) = match source {
        SessionSource::Attached(reader) => return Some(reader),
        SessionSource::Discover { cwd, spawned_at } => (cwd, spawned_at),
    };
    let watch = discover::watch_for_new_session(&cwd, spawned_at);
    let session = match tokio::time::timeout(DISCOVER_TIMEOUT, watch).await {
        Ok(path) => path,
        Err(_) => match discover::discover_session(&cwd) {
            Some(path) => {
                tracing::debug!(?path, "no new session appeared; tailing most recent existing session");
                path
            }
            None => {
                tracing::warn!(?cwd, "no transcript session found; scoring side channel idle");
                return None;
            }
        },
    };
    match TailReader::attach(session, Some(discover::project_dir(&cwd))) {
        Ok(reader) => Some(reader),
        Err(err) => {
            tracing::warn!(%err, "transcript tail attach failed; scoring side channel idle");
            None
        }
    }
}

struct RecentBlock {
    norm: String,
    norm_spans: Vec<SpanScore>,
}

pub struct StylingTransform {
    grid: ShadowGrid,
    fin: Finalizer,
    seg: Segmenter,
    hold: Hold,
    styler: Styler,
    scorer: HeuristicScorer,
    caps: Caps,
    theme: Theme,
    hold_ms: Duration,
    last_activity: Option<Instant>,
    burst_open: bool,
    line_sgr: SgrState,
    reverse_used: bool,
    intake: Option<Receiver<ScoredBlock>>,
    recent_scored: VecDeque<RecentBlock>,
    engine: RepaintEngine,
    last_repaint: Option<Instant>,
    veto_policy: VetoPolicy,
    reload: Option<Arc<AtomicBool>>,
}

impl StylingTransform {
    pub fn new(config: &Config, caps: Caps, rows: u16, cols: u16) -> Self {
        let rows = rows.max(1);
        let cols = cols.max(1);
        Self {
            grid: ShadowGrid::new(rows, cols),
            fin: Finalizer::new(rows),
            seg: Segmenter::new(),
            hold: Hold::new(),
            styler: Styler::new(config.theme.clone(), caps, config.density_budget),
            scorer: HeuristicScorer::new(config),
            caps,
            theme: config.theme.clone(),
            hold_ms: Duration::from_millis(config.hold_ms),
            last_activity: None,
            burst_open: false,
            line_sgr: SgrState::default(),
            reverse_used: false,
            intake: None,
            recent_scored: VecDeque::new(),
            engine: RepaintEngine::new(),
            last_repaint: None,
            veto_policy: config.llm_can_veto.clone(),
            reload: None,
        }
    }

    pub fn attach_score_channel(&mut self, intake: Receiver<ScoredBlock>) {
        self.intake = Some(intake);
    }

    pub fn watch_reload_flag(&mut self, flag: Arc<AtomicBool>) {
        self.reload = Some(flag);
    }

    fn maybe_reload_config(&mut self) {
        let reload_requested = self
            .reload
            .as_ref()
            .is_some_and(|flag| flag.swap(false, Ordering::Relaxed));
        if !reload_requested {
            return;
        }
        let config = Config::load();
        self.styler = Styler::new(config.theme.clone(), self.caps, config.density_budget);
        self.scorer = HeuristicScorer::new(&config);
        self.theme = config.theme.clone();
        self.hold_ms = Duration::from_millis(config.hold_ms);
        self.veto_policy = config.llm_can_veto.clone();
        tracing::info!(
            hold_ms = config.hold_ms,
            density_budget = config.density_budget,
            "SIGHUP: config reloaded into emission pipeline (async scorer keeps spawn-time config)"
        );
    }

    pub fn scored_block_count(&self) -> usize {
        self.recent_scored.len()
    }

    pub fn repaint_pending_rows(&self) -> usize {
        self.engine.pending_rows()
    }

    fn drain_intake(&mut self) {
        let mut incoming = Vec::new();
        let mut disconnected = false;
        if let Some(intake) = &self.intake {
            loop {
                match intake.try_recv() {
                    Ok(block) => incoming.push(block),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            tracing::debug!("score channel disconnected; continuing heuristics-only");
            self.intake = None;
        }
        for block in incoming {
            self.note_scored(block);
        }
    }

    fn note_scored(&mut self, block: ScoredBlock) {
        if block.spans.is_empty() {
            tracing::debug!(
                message_id = %block.message_id,
                block_index = block.block_index,
                "scored block has no spans; nothing to refine"
            );
            return;
        }
        let window = self.harvest_window();
        let cell_spans = map_spans(&block.clean, &block.spans, &window, &CLAUDE_GUTTERS);
        if !cell_spans.is_empty() {
            self.engine.queue(&mut self.grid, cell_spans);
        }
        let (norm, map) = normalize_markdown(&block.clean);
        let norm_spans = block
            .spans
            .iter()
            .filter_map(|span| {
                let range = project_norm_range(&map, span);
                range.map(|range| SpanScore::new(range, span.facet.clone(), span.weight, span.source))
            })
            .collect();
        self.recent_scored.push_back(RecentBlock { norm, norm_spans });
        if self.recent_scored.len() > RECENT_SCORED_CAP {
            self.recent_scored.pop_front();
        }
        tracing::debug!(
            message_id = %block.message_id,
            block_index = block.block_index,
            spans = block.spans.len(),
            rows_queued = self.engine.pending_rows(),
            "scored block ingested for merge and repaint"
        );
    }

    fn harvest_window(&mut self) -> Vec<(AbsRow, String)> {
        let (rows, _) = self.grid.size();
        let base = self.grid.scroll_base();
        let start = base.saturating_sub(u64::from(rows));
        let end = base + u64::from(rows);
        (start..end)
            .filter_map(|abs| {
                let row = AbsRow(abs);
                self.grid.row_text(row).map(|text| (row, text))
            })
            .collect()
    }

    fn maybe_repaint(&mut self, now: Instant, out: &mut Vec<u8>) {
        if self.engine.pending_rows() == 0 || !self.hold.is_empty() {
            return;
        }
        let child_quiet = self
            .last_activity
            .is_none_or(|last| now.duration_since(last) >= QUIET_GAP);
        if !child_quiet {
            return;
        }
        if self
            .last_repaint
            .is_some_and(|last| now.duration_since(last) < REPAINT_MIN_INTERVAL)
        {
            return;
        }
        if let Some(batch) = self.engine.build_batch(&mut self.grid, &self.fin, &self.caps, &self.theme) {
            self.last_repaint = Some(now);
            out.extend_from_slice(&batch);
        }
    }

    pub fn note_virtual_quiet(&mut self, out: &mut Vec<u8>) {
        if !self.caps.styling_enabled {
            return;
        }
        self.reverse_used = false;
        self.end_burst();
        self.drain_ready(out);
    }

    fn end_burst(&mut self) {
        self.fin.note_burst_end();
        self.grid.telemetry_mut().end_burst();
        self.burst_open = false;
    }

    fn ingest_events(&mut self) {
        let events = self.grid.telemetry_mut().take_events();
        self.fin.ingest(events);
    }

    fn process_text_piece(&mut self, piece: &[u8], now: Instant, out: &mut Vec<u8>) {
        if self.hold.is_line_start() {
            self.line_sgr = self.grid.telemetry().live_sgr().clone();
        }
        self.grid.process(piece);
        self.ingest_events();
        if self.grid.alt_screen_active() {
            self.flush_all(out);
            out.extend_from_slice(piece);
            return;
        }
        let deadline = now + self.hold_ms;
        if piece.last() == Some(&b'\n') {
            let (cursor_row, _) = self.grid.telemetry().cursor();
            let abs_after = self.grid.telemetry().scrolled_total() + u64::from(cursor_row);
            let row = AbsRow(abs_after.saturating_sub(1));
            let generation = self.grid.row_generation(row);
            if self
                .hold
                .complete_line(piece, row, generation, self.line_sgr.clone(), deadline)
            {
                out.extend_from_slice(piece);
            }
            self.drain_ready(out);
        } else if self.hold.push_partial(piece, deadline) {
            out.extend_from_slice(piece);
        }
    }

    fn drain_ready(&mut self, out: &mut Vec<u8>) {
        while let Some(front) = self.hold.front() {
            if self.fin.classify(front.row) != Finality::ScrollbackBound {
                break;
            }
            let line = self.hold.pop_front().expect("front exists");
            self.emit_resolved(line, out);
        }
    }

    fn emit_resolved(&mut self, line: HeldLine, out: &mut Vec<u8>) {
        let verdict = self.fin.classify(line.row);
        if verdict == Finality::ScrollbackBound
            && self.fin.is_still_valid(&self.grid, line.row, line.generation)
        {
            let allow_reverse = !self.reverse_used;
            let styled = self.styler.style_line(&line.bytes, &line.sgr_before, allow_reverse, |clean| {
                let heuristic = self.scorer.score_line(clean);
                let llm = merge_cached_spans(&self.recent_scored, clean);
                cascade::merge(&heuristic, &llm, &self.veto_policy)
            });
            if let Some(styled) = styled {
                tracing::trace!(row = line.row.0, "line emitted styled");
                self.reverse_used |= styled.used_reverse;
                out.extend_from_slice(&styled.bytes);
                return;
            }
        } else {
            tracing::trace!(row = line.row.0, ?verdict, "line emitted raw");
        }
        out.extend_from_slice(&line.bytes);
    }

    fn flush_all(&mut self, out: &mut Vec<u8>) {
        while let Some(line) = self.hold.pop_front() {
            self.emit_resolved(line, out);
        }
        let partial = self.hold.take_partial_raw();
        out.extend_from_slice(&partial);
    }

    fn enforce_cap(&mut self, out: &mut Vec<u8>) {
        while self.hold.held_bytes() > HOLD_CAP {
            let Some(line) = self.hold.pop_front() else {
                break;
            };
            self.emit_resolved(line, out);
        }
    }
}

fn project_norm_range(map: &[usize], span: &SpanScore) -> Option<std::ops::Range<usize>> {
    let start = map.partition_point(|&origin| origin < span.range.start);
    let end = map.partition_point(|&origin| origin < span.range.end);
    (start < end).then_some(start..end)
}

fn strip_gutter(line: &str) -> (usize, &str) {
    for gutter in CLAUDE_GUTTERS {
        if let Some(rest) = line.strip_prefix(gutter) {
            return (gutter.len(), rest);
        }
    }
    (0, line)
}

fn merge_cached_spans(recent: &VecDeque<RecentBlock>, line_clean: &str) -> Vec<SpanScore> {
    if recent.is_empty() {
        return Vec::new();
    }
    let (gutter_len, body) = strip_gutter(line_clean);
    let (line_norm, line_map) = normalize_markdown(body);
    if line_norm.len() < MERGE_MIN_NORM_LEN {
        return Vec::new();
    }
    let mut located: Option<(&RecentBlock, usize)> = None;
    for block in recent {
        let mut hits = memmem::find_iter(block.norm.as_bytes(), line_norm.as_bytes());
        let Some(first) = hits.next() else {
            continue;
        };
        if hits.next().is_some() || located.is_some() {
            tracing::debug!(len = line_norm.len(), "emission merge skipped: line text ambiguous in scored blocks");
            return Vec::new();
        }
        located = Some((block, first));
    }
    let Some((block, base)) = located else {
        return Vec::new();
    };
    let window = base..base + line_norm.len();
    let mut merged = Vec::new();
    for span in &block.norm_spans {
        let overlap = span.range.start.max(window.start)..span.range.end.min(window.end);
        if overlap.start >= overlap.end {
            continue;
        }
        let start = gutter_len + line_map[overlap.start - base];
        let end = gutter_len + line_map[overlap.end - base - 1] + 1;
        merged.push(SpanScore::new(start..end, span.facet.clone(), span.weight, span.source));
    }
    if !merged.is_empty() {
        tracing::trace!(spans = merged.len(), "cached LLM spans merged into emission styling");
    }
    merged
}

impl ChunkTransform for StylingTransform {
    fn transform(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        if !self.caps.styling_enabled {
            out.extend_from_slice(chunk);
            return;
        }
        self.drain_intake();
        let now = Instant::now();
        self.reverse_used = false;
        if self.burst_open
            && self
                .last_activity
                .is_some_and(|last| now.duration_since(last) >= QUIET_GAP)
        {
            self.end_burst();
            self.drain_ready(out);
        }
        self.last_activity = Some(now);
        self.burst_open = true;
        for run in self.seg.feed(chunk) {
            match run.kind {
                RunKind::Ctrl => {
                    self.flush_all(out);
                    self.grid.process(&run.bytes);
                    self.ingest_events();
                    out.extend_from_slice(&run.bytes);
                }
                RunKind::Text => {
                    let mut rest = run.bytes.as_slice();
                    while !rest.is_empty() {
                        let split = memchr::memchr(b'\n', rest).map_or(rest.len(), |i| i + 1);
                        let (piece, tail) = rest.split_at(split);
                        rest = tail;
                        self.process_text_piece(piece, now, out);
                    }
                }
            }
        }
        self.enforce_cap(out);
    }

    fn flush(&mut self, out: &mut Vec<u8>) {
        if !self.caps.styling_enabled {
            return;
        }
        self.maybe_reload_config();
        self.drain_intake();
        let now = Instant::now();
        self.reverse_used = false;
        if self.burst_open
            && self
                .last_activity
                .is_some_and(|last| now.duration_since(last) >= QUIET_GAP)
        {
            self.end_burst();
        }
        while let Some(line) = self.hold.pop_expired(now) {
            self.emit_resolved(line, out);
        }
        if let Some(partial) = self.hold.partial_expired(now) {
            out.extend_from_slice(&partial);
        }
        self.drain_ready(out);
        self.maybe_repaint(now, out);
    }

    fn resize(&mut self, rows: u16, cols: u16, out: &mut Vec<u8>) {
        if !self.caps.styling_enabled {
            return;
        }
        self.reverse_used = false;
        self.flush_all(out);
        self.engine = RepaintEngine::new();
        let rows = rows.max(1);
        let cols = cols.max(1);
        self.grid.resize(rows, cols);
        self.fin.resize(rows);
    }

    fn finish(&mut self, out: &mut Vec<u8>) {
        if !self.caps.styling_enabled {
            return;
        }
        self.reverse_used = false;
        if self.burst_open {
            self.end_burst();
        }
        self.flush_all(out);
        let pending = self.seg.take_pending();
        out.extend_from_slice(&pending);
    }
}
