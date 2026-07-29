# salcc architecture

Read this before touching `term/` or `emit/`. It explains the data flow, the
two styling channels, why the finalization detector is the correctness
linchpin, and the invariants that every change must preserve. Decisions with
rationale live in `docs/decisions/`; evidence behind the theme defaults lives
in `docs/research.md`.

## Data flow

```
user stdin ──raw, never held──► PTY master ──► child (claude/codex/…)
child output ──► vt100 shadow grid + vte telemetry (cursor/scroll/SGR events)
             ──► finalization detector (scrollback-bound vs CC's Ink active-repaint region)
             ──► segmenter (TEXT runs vs CTRL runs) ──► hold buffer (≤150ms, line-atomic)
             ──► emission styler (heuristics NOW + merged LLM spans) ──► real tty
side channel (own thread, current-thread tokio runtime):
  transcript JSONL tail ──► score cache ──► anthropic scorer (async)
             ──► std mpsc ──► transform drains via try_recv (never awaits)
             ──► (a) span→grid mapper ──► repaint queue ──► visible-region
                  repaint batch (flush tick; hold drained ∧ child-quiet
                  ≥100ms ∧ ≥100ms since last batch)
             ──► (b) recent-block store (8) ──► emission merge for lines
                  styled after their block was scored
emission scoring: heuristic spans + merged LLM spans ──► score/cascade merge
  (same-facet overlap: max weight wins; zero-weight LLM span vetoes a
  heuristic span where config llm_can_veto allows — NEVER Risk) ──► styler
config reload: SIGHUP ──► signal_hook flag ──► transform re-runs
  Config::load() at the next flush tick (theme, density, hold_ms, facet
  toggles, dimensions, veto policy; the async scorer keeps spawn-time config)
cost metrics: every API call logged by the scorer, every cache hit logged
  as a zero-token zero-usd record by the side channel ──► metrics.jsonl ──►
  `salcc stats` table / `salcc stats --serve` localhost dashboard
```

The shadow grid (`term/grid`) is the single source of truth about what is on
screen. Everything else — finalization, span mapping, repaint — reads from it.
The same byte stream also feeds a raw `vte::Perform` (`term/telemetry`) for
the low-level CUP/ED/scroll/SGR events that `vt100` abstracts away; the
finalizer consumes those (see ADR 0002).

The io_loop seam (`ChunkTransform`) carries four hooks: `transform` (per
chunk), `flush` (50ms tick, so the hold can resolve deadlines and quiet
gaps end bursts), `resize` (SIGWINCH mirrored via a flag the pump polls;
the styling transform flushes its hold under the old geometry before
resizing grid + finalizer), and `finish` (channel disconnect drains held
bytes so child exit never swallows output). `emit/pipeline.rs` implements
the styling transform: TEXT runs feed the grid newline-piece by
newline-piece so each completed line captures the AbsRow + generation it
landed on; held lines resolve on the first of scroll-finalization (eager),
deadline, CTRL boundary, overflow, resize, or stream end — styled only
when ScrollbackBound and generation-still-valid, raw otherwise. Non-tty
stdout, `NO_COLOR`, `--no-style`, and unknown TERM select the identity
transform outright (`term/caps.rs`).

## Two styling channels, one truth

Styling happens twice, through different paths, against the same grid:

1. **Emission channel** (`emit/`, primary). Styles finalized text lines
   inline as they stream through the proxy — heuristic scores are computed
   synchronously, cached LLM spans are merged if already available. This is
   the *only* chance to style text that scrolls out of the visible region
   immediately, because of scrollback physics: **ANSI cannot repaint rows
   that have scrolled off-screen.** Once a row enters scrollback, the bytes
   we emitted are what the user will ever see.
2. **Repaint channel** (`term/repaint`, refinement). Haiku spans arrive
   ~0.5–1.5s after a block completes (the "Haiku race", below). Spans that
   map to rows still **visible ∧ final ∧ text-unchanged** are applied by
   repainting those rows in place. Batches are wrapped in synchronized
   update (CSI ?2026), throttled to ≤1 repaint burst per 100ms, and the
   cursor is restored from the shadow grid afterward.

The sync/async bridge (`emit/pipeline.rs`): the ChunkTransform runs
synchronously on the io_loop thread while scoring is tokio-async, so
`spawn_side_channel` runs a dedicated thread hosting a current-thread
tokio runtime (session discovery → tail → cache → scorer) and delivers
`(TextBlock, Arc<Vec<SpanScore>>)` over a std mpsc channel. The transform
drains it with a `try_recv` loop at the top of `transform`/`flush` — the
hot path never awaits, and a disconnected channel silently degrades to
heuristics-only. Each scored block feeds both channels: its spans are
mapped against the current grid window (visible rows plus one screen of
scrollback margin — build_batch drops anything no longer visible) and
queued for repaint, and the block itself is kept (most recent 8) so a
held line whose normalized clean text occurs exactly once across kept
blocks gets the overlapping spans projected onto it at emission through
the shared `normalize_markdown` source maps; any ambiguity skips the
merge. Repaint bytes are appended from the flush tick only after the
hold has fully drained, never interleaved inside a held line. `salcc
claude` wires this up when the configured scorer is `anthropic`
(`cli.rs`, SystemTime captured before child spawn for discovery); a
missing API key logs one warning and runs heuristics-only.

The repaint channel must never touch Claude Code's active repaint region:
rows at or below the dynamic-region top (the minimum cursor row of the last
repaint burst) belong to Ink and are rewritten at will by the child.

## Finalization detector (`term/finalize.rs` — correctness linchpin)

Claude Code renders through Ink, which continuously rewrites a dynamic region
at the bottom of the screen (spinner, streaming text, input box). Styling a
row the child is about to rewrite is wasted at best and corrupting at worst.
The finalizer decides which rows are safe:

- A row is **final** when it scrolled the screen at print time (it entered
  scrollback-bound territory), or when it sits above Ink's dynamic-region
  top.
- The transcript tail confirms finality: when a block's text appears in the
  session JSONL, the matching grid rows are final — the child has committed
  that content.
- **False-final safety**: if the child rewrites a row we styled, the child's
  write wins and our overlay for that row is invalidated. Worst case is one
  visible flicker; never corruption. This property is what makes finalizer
  bugs degrade gracefully instead of catastrophically — preserve it.

Zero false-finals across the fixture corpus is an M1 acceptance criterion.
If you change the finalizer, run the conformance suite and check the
false-final assertions before claiming success.

## The Haiku race

Scoring is asynchronous and slower than terminal output:

1. The child streams a response block; the emission channel styles it with
   heuristics as it passes through.
2. The transcript tail (`tail/`, polling stat every 75ms + offset read)
   picks up the completed content block from
   `~/.claude/projects/<cwd with / → ->/<session>.jsonl`. Assistant turns
   are one JSONL line per content block (thinking/text/tool_use, shared
   `message.id`), so scoring starts per completed block, before the whole
   message finishes — this shortens the race. `isSidechain: true` lines are
   subagent output: skip. Unknown `type` values must parse tolerantly.
3. The Anthropic scorer POSTs the clean block text to Haiku; spans come back
   as verbatim quotes (ADR 0003) ~0.5–1.5s later.
4. The span mapper (`map/`) resolves quotes to grid cells; the repaint queue
   applies them to rows that are still visible ∧ final ∧ text-unchanged.
   Rows that already scrolled out keep their heuristic-only styling — that
   is an accepted loss, not a bug.

M3 acceptance: Haiku spans repaint ≤2.5s after block completion, and
repaints never hit non-final or scrolled-out rows (asserted on the byte
log).

## Hard invariants

These are restated with rationale in CLAUDE.md; the load-bearing ones for
`term/` and `emit/` work:

1. **Attrs-only styling** — never change cell text or line width. Sole
   audited exception: a facet glyph replacing a 2-space gutter pad.
2. **Never hold stdin** — keystrokes forward with zero buffering.
3. **Never touch the child's active repaint region** — only visible ∧ final
   ∧ text-unchanged rows may be repainted; child rewrites always win.
4. **Fail-safe span dropping** — ambiguous span→grid match = drop the span.
5. **Bit-perfect passthrough** under `NO_COLOR`, `--no-style`, alt-screen,
   or unknown terminal.
6. **Termios restore guaranteed** on every exit path, including panic.
7. **Precision floors** — heuristic rules <0.85 precision are dim-only or
   off; RISK spans can never be vetoed by the LLM.
8. **Never blink**; reverse video only for the single most critical RISK
   span.
9. **Evidence-locked theme defaults** — density 0.12, dim-first,
   luminance = importance, hue = category. Change only with cause
   documented in docs/research.md.

## Verification hooks

- Fixtures: `salcc record -o tests/fixtures/name.cast -- <cmd>` captures a
  raw session; conformance tests replay every fixture through the proxy and
  snapshot-compare grids (insta). M0 demands byte identity; M2+ demands
  attrs-only deltas (grid *text* identical styled vs `--no-style`),
  enforced across every fixture by tests/styling.rs.
- `salcc replay <cast>` runs a fixture through the styling transform
  offline: cast timestamp gaps (>100ms) drive burst ends, wall-clock hold
  deadlines never fire, verdicts settle at EOF. It shows what the emission
  channel *can* style (scrolled rows, evidence-final rows), which is why
  Ink-region commits replay raw — those belong to the M3 repaint channel.
- Latency budget: p99 < 1ms per chunk through the hot path
  (`cargo bench --bench passthrough`).
- If the data flow in this document changes, updating it is part of the
  milestone's definition of done.
