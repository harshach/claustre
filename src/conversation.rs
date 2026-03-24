//! JSONL conversation parser for Claude Code session logs.
//!
//! Parses `~/.claude/projects/{hashed-path}/{session-id}.jsonl` files into
//! structured [`ConversationEntry`] variants. Supports incremental parsing
//! via byte offsets so callers can resume from where they left off.

use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

/// Maximum characters to keep from a tool result's output.
const TOOL_RESULT_PREVIEW_LEN: usize = 2000;

/// A structured entry parsed from a Claude Code JSONL conversation log.
#[derive(Debug, Clone)]
pub enum ConversationEntry {
    /// A human message (user-typed text).
    UserMessage { timestamp: String, text: String },
    /// An assistant text response block.
    AssistantText { timestamp: String, text: String },
    /// A thinking block (extended thinking). Only the character count is stored.
    Thinking {
        timestamp: String,
        char_count: usize,
    },
    /// A tool call from the assistant.
    ToolUse {
        timestamp: String,
        tool_name: String,
        /// Compact human-readable summary of what the tool call does.
        description: String,
        tool_use_id: String,
    },
    /// A tool result returned to the assistant (from a user-type message).
    ToolResult {
        timestamp: String,
        tool_use_id: String,
        /// First ~2000 characters of the result content.
        output_preview: String,
        is_error: bool,
    },
    /// Marks the end of an assistant turn with optional token usage.
    TurnEnd {
        timestamp: String,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
    },
}

/// Resolve the JSONL path for a given worktree.
///
/// Hashes `worktree_path` using Claude Code's algorithm (replace non-alphanumeric
/// chars with `-`) and looks in `~/.claude/projects/{hash}/`.
///
/// If `claude_session_id` is given, returns the path to that specific session's
/// JSONL file. Otherwise, returns the most recently modified `.jsonl` in the
/// directory.
pub fn resolve_jsonl_path(worktree_path: &str, claude_session_id: Option<&str>) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let hash = hash_path(worktree_path);
    let project_dir = home.join(".claude/projects").join(&hash);

    if !project_dir.is_dir() {
        return None;
    }

    if let Some(session_id) = claude_session_id {
        let jsonl_path = project_dir.join(format!("{session_id}.jsonl"));
        if jsonl_path.is_file() {
            return Some(jsonl_path);
        }
        // session ID file not found — fall through to most-recent lookup
    }

    // No session ID or file not found — find the most recently modified JSONL file.
    most_recent_jsonl(&project_dir)
}

/// Parse a JSONL file starting from `offset` bytes.
///
/// Returns the parsed entries and the new byte offset (for incremental reads).
/// Skips non-conversation types (`progress`, `file-history-snapshot`, etc.).
pub fn parse_jsonl(path: &Path, offset: u64) -> Result<(Vec<ConversationEntry>, u64)> {
    let file = fs::File::open(path)
        .with_context(|| format!("failed to open JSONL: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(offset))
        .with_context(|| format!("failed to seek to offset {offset}"))?;

    let mut entries = Vec::new();
    let mut current_offset = offset;

    let mut line_buf = String::new();
    loop {
        line_buf.clear();
        let bytes_read = reader.read_line(&mut line_buf)?;
        if bytes_read == 0 {
            break; // EOF
        }
        current_offset += bytes_read as u64;

        let trimmed = line_buf.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(entry) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };

        let msg_type = entry.get("type").and_then(Value::as_str).unwrap_or("");
        let timestamp = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        match msg_type {
            "user" => parse_user_message(&entry, &timestamp, &mut entries),
            "assistant" => parse_assistant_message(&entry, &timestamp, &mut entries),
            // Skip progress, file-history-snapshot, system, queue-operation, etc.
            _ => {}
        }
    }

    Ok((entries, current_offset))
}

/// Hash a filesystem path using Claude Code's algorithm:
/// replace every non-alphanumeric character with `-`.
fn hash_path(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Find the most recently modified `.jsonl` file in a directory.
fn most_recent_jsonl(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;

    entries
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "jsonl")
        })
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((e.path(), mtime))
        })
        .max_by_key(|(_, mtime)| *mtime)
        .map(|(path, _)| path)
}

/// Parse a `type: "user"` JSONL line into conversation entries.
fn parse_user_message(entry: &Value, timestamp: &str, entries: &mut Vec<ConversationEntry>) {
    let Some(content) = entry.get("message").and_then(|m| m.get("content")) else {
        return;
    };

    // Content can be a plain string (simple text) or an array of blocks.
    if let Some(text) = content.as_str() {
        if !text.is_empty() {
            entries.push(ConversationEntry::UserMessage {
                timestamp: timestamp.to_string(),
                text: text.to_string(),
            });
        }
        return;
    }

    let Some(blocks) = content.as_array() else {
        return;
    };

    let mut text_parts = Vec::new();

    for block in blocks {
        let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    text_parts.push(text.to_string());
                }
            }
            "tool_result" => {
                let tool_use_id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let is_error = block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let output_preview = extract_tool_result_preview(block);

                entries.push(ConversationEntry::ToolResult {
                    timestamp: timestamp.to_string(),
                    tool_use_id,
                    output_preview,
                    is_error,
                });
            }
            _ => {}
        }
    }

    if !text_parts.is_empty() {
        entries.push(ConversationEntry::UserMessage {
            timestamp: timestamp.to_string(),
            text: text_parts.join("\n"),
        });
    }
}

/// Parse a `type: "assistant"` JSONL line into conversation entries.
fn parse_assistant_message(entry: &Value, timestamp: &str, entries: &mut Vec<ConversationEntry>) {
    let Some(message) = entry.get("message") else {
        return;
    };
    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
        return;
    };

    for block in blocks {
        let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    entries.push(ConversationEntry::AssistantText {
                        timestamp: timestamp.to_string(),
                        text: text.to_string(),
                    });
                }
            }
            "thinking" => {
                let char_count = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .map_or(0, str::len);
                entries.push(ConversationEntry::Thinking {
                    timestamp: timestamp.to_string(),
                    char_count,
                });
            }
            "tool_use" => {
                let tool_name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let tool_use_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                let description = input_summary(&tool_name, &input);

                entries.push(ConversationEntry::ToolUse {
                    timestamp: timestamp.to_string(),
                    tool_name,
                    description,
                    tool_use_id,
                });
            }
            _ => {}
        }
    }

    // Emit a TurnEnd if the message has a `usage` field.
    if let Some(usage) = message.get("usage") {
        let input_tokens = usage.get("input_tokens").and_then(Value::as_i64);
        let output_tokens = usage.get("output_tokens").and_then(Value::as_i64);
        entries.push(ConversationEntry::TurnEnd {
            timestamp: timestamp.to_string(),
            input_tokens,
            output_tokens,
        });
    }
}

/// Produce a compact human-readable summary of a tool call's input.
fn input_summary(tool_name: &str, input: &Value) -> String {
    match tool_name {
        "Bash" => {
            // Prefer the description field; fall back to truncated command.
            if let Some(desc) = input.get("description").and_then(Value::as_str)
                && !desc.is_empty()
            {
                return truncate(desc, 120);
            }
            if let Some(cmd) = input.get("command").and_then(Value::as_str) {
                return truncate(cmd, 120);
            }
            "Bash".to_string()
        }
        "Read" | "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => input
            .get("file_path")
            .and_then(Value::as_str)
            .unwrap_or("(unknown file)")
            .to_string(),
        "Grep" => {
            let pattern = input.get("pattern").and_then(Value::as_str).unwrap_or("?");
            let path = input.get("path").and_then(Value::as_str).unwrap_or(".");
            format!("{pattern} in {path}")
        }
        "Glob" => input
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("*")
            .to_string(),
        "Agent" => {
            if let Some(desc) = input.get("description").and_then(Value::as_str) {
                return truncate(desc, 120);
            }
            if let Some(prompt) = input.get("prompt").and_then(Value::as_str) {
                return truncate(prompt, 120);
            }
            "Agent".to_string()
        }
        "TodoWrite" => {
            // Show the number of todos if available.
            if let Some(todos) = input.get("todos").and_then(Value::as_array) {
                format!("{} todo(s)", todos.len())
            } else {
                "TodoWrite".to_string()
            }
        }
        _ => {
            // Generic: show tool name + first string field value (truncated).
            if let Some(obj) = input.as_object() {
                for value in obj.values() {
                    if let Some(s) = value.as_str() {
                        return truncate(s, 80);
                    }
                }
            }
            tool_name.to_string()
        }
    }
}

/// Extract a preview from a `tool_result` block's content.
///
/// The content field can be a string or an array of content blocks.
fn extract_tool_result_preview(block: &Value) -> String {
    let Some(content) = block.get("content") else {
        return String::new();
    };

    if let Some(text) = content.as_str() {
        return truncate(text, TOOL_RESULT_PREVIEW_LEN);
    }

    if let Some(blocks) = content.as_array() {
        for inner in blocks {
            if let Some(text) = inner.get("text").and_then(Value::as_str) {
                return truncate(text, TOOL_RESULT_PREVIEW_LEN);
            }
        }
    }

    String::new()
}

/// Truncate a string to at most `max_chars` characters, appending `...` if truncated.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s.to_string();
    }
    // Find a char boundary to avoid splitting a multi-byte character.
    let boundary = s
        .char_indices()
        .take_while(|(i, _)| *i < max_chars)
        .last()
        .map_or(0, |(i, c)| i + c.len_utf8());
    let mut result = s[..boundary].to_string();
    result.push_str("...");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── hash_path ──

    #[test]
    fn hash_path_replaces_non_alphanumeric() {
        assert_eq!(
            hash_path("/Users/harsha/Code/claustre"),
            "-Users-harsha-Code-claustre"
        );
    }

    #[test]
    fn hash_path_preserves_alphanumeric() {
        assert_eq!(hash_path("abc123"), "abc123");
    }

    #[test]
    fn hash_path_handles_dots_and_spaces() {
        assert_eq!(hash_path("my project.rs"), "my-project-rs");
    }

    // ── truncate ──

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string_adds_ellipsis() {
        let result = truncate("hello world", 5);
        assert_eq!(result, "hello...");
    }

    #[test]
    fn truncate_exact_boundary() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    // ── input_summary ──

    #[test]
    fn input_summary_bash_prefers_description() {
        let input = serde_json::json!({
            "command": "cargo build 2>&1",
            "description": "Build the project"
        });
        assert_eq!(input_summary("Bash", &input), "Build the project");
    }

    #[test]
    fn input_summary_bash_falls_back_to_command() {
        let input = serde_json::json!({ "command": "ls -la" });
        assert_eq!(input_summary("Bash", &input), "ls -la");
    }

    #[test]
    fn input_summary_read_shows_file() {
        let input = serde_json::json!({ "file_path": "/src/main.rs" });
        assert_eq!(input_summary("Read", &input), "/src/main.rs");
    }

    #[test]
    fn input_summary_grep_shows_pattern_and_path() {
        let input = serde_json::json!({ "pattern": "TODO", "path": "/src" });
        assert_eq!(input_summary("Grep", &input), "TODO in /src");
    }

    #[test]
    fn input_summary_glob_shows_pattern() {
        let input = serde_json::json!({ "pattern": "**/*.rs" });
        assert_eq!(input_summary("Glob", &input), "**/*.rs");
    }

    #[test]
    fn input_summary_agent_prefers_description() {
        let input = serde_json::json!({
            "description": "Find launch modal code",
            "prompt": "Search the codebase..."
        });
        assert_eq!(input_summary("Agent", &input), "Find launch modal code");
    }

    #[test]
    fn input_summary_edit_shows_file() {
        let input = serde_json::json!({
            "file_path": "/src/lib.rs",
            "old_string": "foo",
            "new_string": "bar"
        });
        assert_eq!(input_summary("Edit", &input), "/src/lib.rs");
    }

    #[test]
    fn input_summary_unknown_tool_shows_first_string() {
        let input = serde_json::json!({ "query": "what is claustre?" });
        assert_eq!(input_summary("WebSearch", &input), "what is claustre?");
    }

    #[test]
    fn input_summary_unknown_tool_no_input() {
        assert_eq!(input_summary("WebSearch", &Value::Null), "WebSearch");
    }

    // ── extract_tool_result_preview ──

    #[test]
    fn tool_result_preview_string_content() {
        let block = serde_json::json!({
            "type": "tool_result",
            "tool_use_id": "abc",
            "content": "Here is the file content..."
        });
        assert_eq!(
            extract_tool_result_preview(&block),
            "Here is the file content..."
        );
    }

    #[test]
    fn tool_result_preview_truncates_long_content() {
        let long = "x".repeat(3000);
        let block = serde_json::json!({
            "type": "tool_result",
            "tool_use_id": "abc",
            "content": long
        });
        let preview = extract_tool_result_preview(&block);
        assert!(preview.len() <= TOOL_RESULT_PREVIEW_LEN + 3); // +3 for "..."
        assert!(preview.ends_with("..."));
    }

    #[test]
    fn tool_result_preview_missing_content() {
        let block = serde_json::json!({
            "type": "tool_result",
            "tool_use_id": "abc"
        });
        assert_eq!(extract_tool_result_preview(&block), "");
    }

    // ── parse_jsonl (full integration) ──

    /// Helper: write lines to a temp file and parse from offset 0.
    fn parse_lines(lines: &[&str]) -> Vec<ConversationEntry> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        let (entries, _) = parse_jsonl(&path, 0).unwrap();
        entries
    }

    #[test]
    fn parse_user_text_message() {
        let line = serde_json::json!({
            "type": "user",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "role": "user",
                "content": "Hello Claude"
            }
        });
        let entries = parse_lines(&[&line.to_string()]);
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(&entries[0], ConversationEntry::UserMessage { text, .. } if text == "Hello Claude")
        );
    }

    #[test]
    fn parse_user_text_blocks() {
        let line = serde_json::json!({
            "type": "user",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "role": "user",
                "content": [
                    { "type": "text", "text": "First part" },
                    { "type": "text", "text": "Second part" }
                ]
            }
        });
        let entries = parse_lines(&[&line.to_string()]);
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(&entries[0], ConversationEntry::UserMessage { text, .. } if text == "First part\nSecond part")
        );
    }

    #[test]
    fn parse_user_tool_result() {
        let line = serde_json::json!({
            "type": "user",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_abc",
                        "content": "file content here",
                        "is_error": false
                    }
                ]
            }
        });
        let entries = parse_lines(&[&line.to_string()]);
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            &entries[0],
            ConversationEntry::ToolResult {
                tool_use_id,
                output_preview,
                is_error: false,
                ..
            } if tool_use_id == "toolu_abc" && output_preview == "file content here"
        ));
    }

    #[test]
    fn parse_user_tool_result_error() {
        let line = serde_json::json!({
            "type": "user",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_xyz",
                        "content": "The user rejected this tool use.",
                        "is_error": true
                    }
                ]
            }
        });
        let entries = parse_lines(&[&line.to_string()]);
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            &entries[0],
            ConversationEntry::ToolResult { is_error: true, .. }
        ));
    }

    #[test]
    fn parse_assistant_text_and_thinking() {
        let line = serde_json::json!({
            "type": "assistant",
            "timestamp": "2025-01-01T00:00:01Z",
            "message": {
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "Let me think about this..." },
                    { "type": "text", "text": "Here is my answer." }
                ],
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50
                }
            }
        });
        let entries = parse_lines(&[&line.to_string()]);
        // Should produce: Thinking, AssistantText, TurnEnd
        assert_eq!(entries.len(), 3);

        assert!(matches!(
            &entries[0],
            ConversationEntry::Thinking { char_count, .. } if *char_count == "Let me think about this...".len()
        ));
        assert!(
            matches!(&entries[1], ConversationEntry::AssistantText { text, .. } if text == "Here is my answer.")
        );
        assert!(matches!(
            &entries[2],
            ConversationEntry::TurnEnd {
                input_tokens: Some(100),
                output_tokens: Some(50),
                ..
            }
        ));
    }

    #[test]
    fn parse_assistant_tool_use() {
        let line = serde_json::json!({
            "type": "assistant",
            "timestamp": "2025-01-01T00:00:02Z",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_123",
                        "name": "Bash",
                        "input": {
                            "command": "cargo build",
                            "description": "Build the project"
                        }
                    }
                ],
                "usage": {
                    "input_tokens": 200,
                    "output_tokens": 80
                }
            }
        });
        let entries = parse_lines(&[&line.to_string()]);
        // ToolUse + TurnEnd
        assert_eq!(entries.len(), 2);
        assert!(matches!(
            &entries[0],
            ConversationEntry::ToolUse {
                tool_name,
                description,
                tool_use_id,
                ..
            } if tool_name == "Bash" && description == "Build the project" && tool_use_id == "toolu_123"
        ));
        assert!(matches!(&entries[1], ConversationEntry::TurnEnd { .. }));
    }

    #[test]
    fn parse_skips_non_conversation_types() {
        let progress = serde_json::json!({
            "type": "progress",
            "timestamp": "2025-01-01T00:00:00Z",
            "data": {}
        });
        let file_history = serde_json::json!({
            "type": "file-history-snapshot",
            "timestamp": "2025-01-01T00:00:00Z",
            "files": []
        });
        let system = serde_json::json!({
            "type": "system",
            "timestamp": "2025-01-01T00:00:00Z"
        });
        let entries = parse_lines(&[
            &progress.to_string(),
            &file_history.to_string(),
            &system.to_string(),
        ]);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_handles_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        fs::File::create(&path).unwrap();

        let (entries, offset) = parse_jsonl(&path, 0).unwrap();
        assert!(entries.is_empty());
        assert_eq!(offset, 0);
    }

    #[test]
    fn parse_handles_blank_lines_and_invalid_json() {
        let entries = parse_lines(&[
            "",
            "not valid json",
            &serde_json::json!({
                "type": "user",
                "timestamp": "2025-01-01T00:00:00Z",
                "message": { "role": "user", "content": "hello" }
            })
            .to_string(),
            "",
        ]);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn parse_incremental_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incremental.jsonl");
        let mut file = fs::File::create(&path).unwrap();

        let line1 = serde_json::json!({
            "type": "user",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": { "role": "user", "content": "first" }
        });
        let line2 = serde_json::json!({
            "type": "user",
            "timestamp": "2025-01-01T00:01:00Z",
            "message": { "role": "user", "content": "second" }
        });
        writeln!(file, "{}", line1).unwrap();
        writeln!(file, "{}", line2).unwrap();

        // First parse: read everything.
        let (entries, offset1) = parse_jsonl(&path, 0).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(offset1 > 0);

        // Second parse from offset: nothing new.
        let (entries, offset2) = parse_jsonl(&path, offset1).unwrap();
        assert!(entries.is_empty());
        assert_eq!(offset2, offset1);

        // Append a third line.
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        let line3 = serde_json::json!({
            "type": "user",
            "timestamp": "2025-01-01T00:02:00Z",
            "message": { "role": "user", "content": "third" }
        });
        writeln!(file, "{}", line3).unwrap();

        // Third parse from offset1: picks up only the new line.
        let (entries, offset3) = parse_jsonl(&path, offset1).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(&entries[0], ConversationEntry::UserMessage { text, .. } if text == "third")
        );
        assert!(offset3 > offset1);
    }

    #[test]
    fn parse_full_conversation_round_trip() {
        let user_msg = serde_json::json!({
            "type": "user",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": { "role": "user", "content": "Fix the bug in main.rs" }
        });
        let assistant_think_and_tool = serde_json::json!({
            "type": "assistant",
            "timestamp": "2025-01-01T00:00:05Z",
            "message": {
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "I need to read the file first..." },
                    {
                        "type": "tool_use",
                        "id": "toolu_read1",
                        "name": "Read",
                        "input": { "file_path": "/src/main.rs" }
                    }
                ],
                "usage": { "input_tokens": 500, "output_tokens": 100 }
            }
        });
        let tool_result = serde_json::json!({
            "type": "user",
            "timestamp": "2025-01-01T00:00:06Z",
            "message": {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_read1",
                        "content": "fn main() {\n    println!(\"hello\");\n}"
                    }
                ]
            }
        });
        let assistant_reply = serde_json::json!({
            "type": "assistant",
            "timestamp": "2025-01-01T00:00:10Z",
            "message": {
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "I found the issue. Let me fix it." },
                    {
                        "type": "tool_use",
                        "id": "toolu_edit1",
                        "name": "Edit",
                        "input": {
                            "file_path": "/src/main.rs",
                            "old_string": "println!(\"hello\")",
                            "new_string": "println!(\"world\")"
                        }
                    }
                ],
                "usage": { "input_tokens": 1000, "output_tokens": 200 }
            }
        });

        let entries = parse_lines(&[
            &user_msg.to_string(),
            &assistant_think_and_tool.to_string(),
            &tool_result.to_string(),
            &assistant_reply.to_string(),
        ]);

        // Expected sequence:
        // 1. UserMessage ("Fix the bug...")
        // 2. Thinking
        // 3. ToolUse (Read /src/main.rs)
        // 4. TurnEnd (500/100)
        // 5. ToolResult (toolu_read1)
        // 6. AssistantText ("I found the issue...")
        // 7. ToolUse (Edit /src/main.rs)
        // 8. TurnEnd (1000/200)
        assert_eq!(entries.len(), 8);

        assert!(matches!(&entries[0], ConversationEntry::UserMessage { .. }));
        assert!(matches!(&entries[1], ConversationEntry::Thinking { .. }));
        assert!(matches!(
            &entries[2],
            ConversationEntry::ToolUse { tool_name, description, .. }
            if tool_name == "Read" && description == "/src/main.rs"
        ));
        assert!(matches!(&entries[3], ConversationEntry::TurnEnd { .. }));
        assert!(matches!(
            &entries[4],
            ConversationEntry::ToolResult { tool_use_id, .. } if tool_use_id == "toolu_read1"
        ));
        assert!(matches!(
            &entries[5],
            ConversationEntry::AssistantText { .. }
        ));
        assert!(matches!(
            &entries[6],
            ConversationEntry::ToolUse { tool_name, description, .. }
            if tool_name == "Edit" && description == "/src/main.rs"
        ));
        assert!(matches!(
            &entries[7],
            ConversationEntry::TurnEnd {
                input_tokens: Some(1000),
                output_tokens: Some(200),
                ..
            }
        ));
    }

    // ── resolve_jsonl_path ──

    #[test]
    fn resolve_jsonl_path_returns_none_for_nonexistent_dir() {
        // This path won't exist as a hashed Claude projects dir.
        assert!(resolve_jsonl_path("/nonexistent/path/zzz_test_zzz", None).is_none());
    }

    #[test]
    fn most_recent_jsonl_picks_newest() {
        let dir = tempfile::tempdir().unwrap();

        let old = dir.path().join("old.jsonl");
        fs::write(&old, "{}").unwrap();

        // Ensure the second file has a different mtime.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let new = dir.path().join("new.jsonl");
        fs::write(&new, "{}").unwrap();

        let result = most_recent_jsonl(dir.path());
        assert_eq!(result.unwrap().file_name().unwrap(), "new.jsonl");
    }

    #[test]
    fn most_recent_jsonl_ignores_non_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("data.json"), "{}").unwrap();
        fs::write(dir.path().join("session.jsonl"), "{}").unwrap();

        let result = most_recent_jsonl(dir.path());
        assert_eq!(result.unwrap().file_name().unwrap(), "session.jsonl");
    }

    #[test]
    fn most_recent_jsonl_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(most_recent_jsonl(dir.path()).is_none());
    }

    #[test]
    fn parse_assistant_no_usage_skips_turn_end() {
        let line = serde_json::json!({
            "type": "assistant",
            "timestamp": "2025-01-01T00:00:01Z",
            "message": {
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "Hello" }
                ]
            }
        });
        let entries = parse_lines(&[&line.to_string()]);
        // Only AssistantText, no TurnEnd.
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            &entries[0],
            ConversationEntry::AssistantText { .. }
        ));
    }

    #[test]
    fn parse_user_mixed_text_and_tool_results() {
        let line = serde_json::json!({
            "type": "user",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "role": "user",
                "content": [
                    { "type": "text", "text": "User comment" },
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_mixed",
                        "content": "result output"
                    }
                ]
            }
        });
        let entries = parse_lines(&[&line.to_string()]);
        // Should produce both a ToolResult and a UserMessage.
        assert_eq!(entries.len(), 2);
        // ToolResult comes first (processed inline), UserMessage comes after (aggregated text).
        assert!(matches!(&entries[0], ConversationEntry::ToolResult { .. }));
        assert!(
            matches!(&entries[1], ConversationEntry::UserMessage { text, .. } if text == "User comment")
        );
    }

    #[test]
    fn input_summary_todowrite() {
        let input = serde_json::json!({
            "todos": [
                { "id": "1", "content": "Fix bug" },
                { "id": "2", "content": "Add test" }
            ]
        });
        assert_eq!(input_summary("TodoWrite", &input), "2 todo(s)");
    }
}
