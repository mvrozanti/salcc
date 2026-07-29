# 0002 — vt100 shadow grid (plus raw vte telemetry)

Status: accepted

## Context

Styling safely requires knowing exactly what is on screen: cell text, attrs,
cursor, scroll history. Candidates: the `vt100` crate, `alacritty_terminal`,
`termwiz` Surface, or hand-rolling on raw `vte`. `alacritty_terminal` is a
heavyweight internal API that tracks an editor's needs, not a proxy's;
raw-`vte`-only means reimplementing a terminal state machine.

## Decision

Maintain a `vt100` shadow grid as the single source of truth, and feed the
same byte stream through a raw `vte::Perform` in parallel for telemetry —
the CUP/ED/scroll/SGR events `vt100` abstracts away, which the finalization
detector needs.

## Consequences

Cell attributes, scrollback, and grid diffing come for free; the repaint
channel restores cursor state from the grid. The finalizer gets raw event
timing without a custom parser. Risk: `vt100` API drift or unmaintained
status — verify at M1; fallback is `termwiz` Surface behind the same
internal grid trait.
