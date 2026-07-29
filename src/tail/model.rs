//! Tolerant serde model of Claude Code transcript lines. The format is
//! undocumented and changes: every field defaults, unknown `type` values
//! parse into the same struct and are ignored downstream, `message.content`
//! accepts both the assistant block-array shape and the user string shape.
//! Only `type=="assistant"` + `isSidechain==false` + a `content[].type=="text"`
//! block is load-bearing; everything else yields None from `text_blocks`.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBlock {
    pub message_id: String,
    pub block_text: String,
    pub uuid: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct TranscriptLine {
    #[serde(default, rename = "type")]
    pub line_type: String,
    #[serde(default, rename = "isSidechain")]
    pub is_sidechain: bool,
    #[serde(default)]
    pub uuid: String,
    #[serde(default, rename = "parentUuid")]
    pub parent_uuid: Option<String>,
    #[serde(default)]
    pub message: Option<TranscriptMessage>,
}

#[derive(Debug, Default, Deserialize)]
pub struct TranscriptMessage {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub content: MessageContent,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Blocks(Vec<ContentBlock>),
    Text(String),
    Other(serde_json::Value),
}

impl Default for MessageContent {
    fn default() -> Self {
        MessageContent::Blocks(Vec::new())
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ContentBlock {
    #[serde(default, rename = "type")]
    pub block_type: String,
    #[serde(default)]
    pub text: String,
}

pub fn parse_line(raw: &str) -> Option<TranscriptLine> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match serde_json::from_str(trimmed) {
        Ok(line) => Some(line),
        Err(err) => {
            tracing::debug!(%err, "transcript line unparseable; skipped");
            None
        }
    }
}

pub fn text_blocks(raw: &str) -> Option<TextBlock> {
    let line = parse_line(raw)?;
    if line.line_type != "assistant" || line.is_sidechain {
        return None;
    }
    let message = line.message?;
    let MessageContent::Blocks(blocks) = message.content else {
        return None;
    };
    let block = blocks
        .into_iter()
        .find(|b| b.block_type == "text" && !b.text.is_empty())?;
    Some(TextBlock {
        message_id: message.id,
        block_text: block.text,
        uuid: line.uuid,
    })
}
