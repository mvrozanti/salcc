# 0003 — Quote-based spans over byte offsets

Status: accepted

## Context

The LLM scorer must point at spans inside a text block. Byte or character
offsets fail twice over: models hallucinate arithmetic, and the offset base
is ambiguous — markdown source, normalized text, and the rendered grid all
disagree after wrapping and gutter insertion.

## Decision

Scorers return spans as verbatim quotes: `{quote, occurrence?, facet,
weight}`. A locator resolves each quote against the normalized block text
(then to grid cells via the span mapper). A quote that is absent or
ambiguous after `occurrence` disambiguation is dropped, silently.

## Consequences

The offset-hallucination failure class disappears; a model can only
reference text that actually exists. Drop-on-ambiguity enforces the
fail-safe invariant — salcc never styles the wrong text, at the price of
occasionally styling nothing. Cost: locator complexity (normalization with
source maps, occurrence counting) lives in `src/map/`, and every scorer
backend must emit quotes, keeping the `Scorer` trait uniform.
