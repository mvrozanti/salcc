//! Claude Code transcript tail: the clean-text side channel.
//!
//! Session files live at ~/.claude/projects/<cwd with '/'->'-'>/<uuid>.jsonl.
//! One JSON object per line. Assistant turns are ONE LINE PER CONTENT BLOCK
//! (thinking/text/tool_use) sharing .message.id — a text block line appearing
//! means that block is complete: that is the scoring trigger, before the
//! whole message finishes.
//!
//! Robustness contract: tolerate unknown `type` values (format is
//! undocumented and changes); only `type=="assistant"` with
//! `.message.content[].text` is load-bearing; skip isSidechain==true
//! (subagent transcripts, not rendered in the main scroll); tolerate a torn
//! last line (reader holds offset, retries next poll). Polling, not inotify:
//! stat every 75ms, read from stored offset.

pub mod discover;
pub mod model;
pub mod reader;
