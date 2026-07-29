# ADR 0005: Self-annotation scorer — accepted as future mode, not default

Status: accepted (deferred implementation)

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
