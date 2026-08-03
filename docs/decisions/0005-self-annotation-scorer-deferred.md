# ADR 0005: Self-annotation scorer — accepted as future mode, not default

Status: superseded — implemented as the DEFAULT scorer by user decision
(2026-08-02, milestone M6). The original blockers were resolved or accepted:
the width-invariant concern (reason 4 below) is void because the annotation
line is never stripped — it is concealed on screen via attrs only (SGR 8 +
SGR 2, emission-time, `emit/styler.rs`), so text and width are untouched
and no second audited exception exists. Marker leakage (reason 2) is
accepted as the cost of zero-latency zero-cost scoring: markers persist in
the transcript but render concealed under salcc and merely odd without it.
Self-assessment bias and compliance drift (reasons 1, 3) are accepted
trade-offs; the independent Haiku judge remains available as
`scorer: anthropic`. Implementation: `src/score/self_report.rs`
(`SourceId::SelfReport`, fixed per-facet weights, cross-block quote
resolution in `emit/pipeline.rs`), instruction injected via
`--append-system-prompt` when the child advertises it (`src/cli.rs`).

## Context

The wrapped agent could mark its own spans inline (instructed via output
style / CLAUDE.md to emit lightweight markers like `⟦r:…⟧`), which salcc
detects, strips, and styles. This is the Graphologue pattern (UIST 2023):
annotation during generation. It eliminates scorer latency entirely (markers
arrive in-stream, styleable at emission — fully solves the scrollback race)
and costs no extra API calls; the instruction overhead (~300 tokens) is
negligible.

## Decision

Implement later as `scorer: self-annotate`, a third mode. Haiku stays the
default.

Reasons it cannot be default: (1) self-assessment bias — a confidently-wrong
model will not mark itself UNCERTAIN; an independent judge will
(Vasconcelos-shaped lesson: score what the reader needs, not what the model
believes about itself); (2) marker leakage — markers persist in the
transcript and surface raw in any context without salcc (history readback,
copy-paste, log renderers); (3) compliance drift — sparse-marking discipline
degrades over long sessions and cannot be enforced; (4) stripping markers
changes text width, breaking the attrs-only invariant — requires a second
audited exception (strip only on ScrollbackBound rows, which the child never
cursor-addresses, plus a parallel display-grid bookkeeping for repaint).

## Consequences

Scorer trait needs no change (self-annotate is an emission-path feature, not
a Scorer impl). The width-exception design must be written before
implementation. Only works for agents whose system prompt we control.
