# salcc — LLM Developer Guide

You (an LLM) are the main developer of this codebase. Read this file fully
before any change. Read `docs/architecture.md` before touching `term/` or
`emit/`. Decisions already made live in `docs/decisions/` — read the relevant
ADR instead of re-litigating.

## What this is

A transparent PTY proxy (`salcc claude`) that re-styles LLM agent terminal
output so semantically important spans are visually obvious: syntax
highlighting for *meaning*. Facets: ACTION ▶, RISK ⚠, COMMAND ❯, NOVELTY ◆,
UNCERTAINTY (dim/undercurl), plus DIM for boilerplate.

## Module map

| Path | Responsibility |
|---|---|
| `src/proxy/` | PTY spawn + passthrough hot path (io_loop = every byte flows here) |
| `src/term/` | Shadow vt100 grid, vte telemetry, **finalize.rs = correctness linchpin**, visible-region repaint, capability detection, `salcc doctor` |
| `src/emit/` | TEXT/CTRL segmentation, hold buffer, emission-time styler (SGR merge, density budget) |
| `src/score/` | Scorer trait impls: heuristic (instant), anthropic (Haiku, default), cascade merge, cache |
| `src/map/` | Clean-text span → grid cell mapping (normalize, anchor, quote locate) |
| `src/tail/` | Claude Code transcript JSONL tail (clean text side channel) |
| `src/config/` | YAML config, theme, user-defined dimensions |
| `src/stats/` | Cost metrics JSONL + `salcc stats --serve` dashboard |
| `src/types.rs` | Shared interface types — changing these = interface change, update docs |

## Hard invariants (violating any of these is a bug, full stop)

1. **Attrs-only styling**: never change cell text or line width. Sole audited
   exception: facet glyph replacing a 2-space gutter pad.
2. **Never hold stdin**: user keystrokes forward with zero buffering or
   inspection.
3. **Never touch the child's active repaint region**: only rows that are
   visible ∧ final ∧ text-unchanged may be repainted. Child rewrites always
   win; our overlay invalidates.
4. **Fail-safe span dropping**: ambiguous span→grid match = drop the span.
   Never style the wrong text.
5. **Bit-perfect passthrough** when: `NO_COLOR`, `--no-style`, alt-screen
   active, or unknown terminal.
6. **Termios restore guaranteed** on every exit path including panic.
7. **Precision floors**: heuristic rules < 0.85 estimated precision are
   dim-only or off. LLM prompt stays sparse (precision over recall). RISK
   spans can never be vetoed by the LLM.
8. **Never blink.** Reverse video only for the single most critical RISK span.
9. Evidence-locked theme defaults (density 0.12, dim-first, luminance =
   importance, hue = category): change only with cause documented in
   `docs/research.md`.

## Commands

```sh
cargo check                 # fast validation
cargo nextest run           # tests (or: cargo test)
cargo clippy -- -D warnings
cargo bench --bench passthrough   # latency budget: p99 < 1ms per chunk
cargo insta review          # review grid snapshot changes
cargo run -- doctor --verify      # caps/config/key/transcript diagnosis + PTY self-check
cargo run -- completions zsh      # shell completions (zsh/bash/fish) to stdout
```

Fixture workflow: `salcc record -o tests/fixtures/name.cast -- <cmd>` records
a raw session; conformance tests replay every fixture through the proxy and
snapshot-compare grids. If you change anything in `term/` or `emit/`, run the
conformance suite before claiming success.

## Conventions

- Comments: module-level `//!` contracts only. No inline comments — naming
  and structure must self-document.
- Logs (`tracing`) go to a file, NEVER stdout/stderr (we own the terminal).
  Write log messages so a future LLM session can debug from the log alone.
- Conventional Commits.
- Milestone definition-of-done includes updating `docs/architecture.md` if
  the data flow changed.
