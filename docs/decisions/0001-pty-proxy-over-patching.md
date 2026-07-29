# 0001 — PTY proxy over Claude Code patching

Status: accepted

## Context

Claude Code exposes no rendering extension point. Hooks fire on lifecycle
events but can never modify displayed assistant text; output styles only
change the system prompt; plugins and themes contribute zero rendering APIs.
Feature requests for a rendering/theming plugin API were closed as duplicates
(anthropics/claude-code#42189, #1302, #22367, #25366, #29166, #8504). The
remaining doors: a custom stream-json frontend, an Agent SDK TUI, cli.js
binary patching (tweakcc-style), or wrapping the terminal itself.

## Decision

salcc is a transparent PTY proxy: `salcc <cmd…>` spawns the agent CLI in a
PTY, shadows its output, and re-emits styled bytes to the real terminal.

## Consequences

Agent-agnostic (claude today, codex/gemini profiles later), survives every
Claude Code release, ships as one static binary, and the real UI is never
forked or re-implemented. The cost: salcc must solve terminal physics itself
(shadow grid, finalization detection, repaint safety), and it can only
restyle output — never restructure it.
