use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::agents::claude;

pub struct TranscriptInfo {
    pub model: Option<String>,
    pub branch: Option<String>,
    pub last_prompt: Option<String>,
    pub last_tool: Option<String>,
    pub context_tokens: u64,
}

pub fn transcript_path(cwd: &str, session_id: &str) -> PathBuf {
    claude::transcript_path(cwd, session_id)
}

/// Clean a raw prompt string for display.
/// Claude Code encodes slash commands as XML tags like:
///   <command-message>args</command-message><command-name>/skill</command-name>...
/// We extract the command name + args; for plain text we return as-is.
fn clean_prompt(text: &str) -> String {
    if text.contains("<command-name>") {
        let name = extract_tag(text, "command-name").unwrap_or_default();
        let msg  = extract_tag(text, "command-message").unwrap_or_default();
        if msg.is_empty() {
            name.to_string()
        } else {
            format!("{} {}", name, msg)
        }
    } else {
        // Strip any residual XML tags (e.g. <system-reminder>)
        strip_tags(text)
    }
}

fn extract_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open  = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = text.find(&open)? + open.len();
    let end   = text.find(&close)?;
    Some(&text[start..end])
}

fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    for c in text.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Read the current git branch for the given working directory.
pub fn git_branch(cwd: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !branch.is_empty() && branch != "HEAD" {
            Some(branch)
        } else {
            None
        }
    } else {
        None
    }
}

/// Read the last ~32 KB of the transcript JSONL and extract session info.
pub fn read_latest(path: &Path) -> Option<TranscriptInfo> {
    let mut file = std::fs::File::open(path).ok()?;
    let file_size = file.metadata().ok()?.len();
    let tail_size = file_size.min(32 * 1024);

    if tail_size > 0 {
        file.seek(SeekFrom::End(-(tail_size as i64))).ok()?;
    }

    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;

    let mut model: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut last_prompt: Option<String> = None;
    let mut last_tool: Option<String> = None;
    let mut context_tokens: u64 = 0;

    for line in buf.lines() {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        if let Some(b) = val["gitBranch"].as_str()
            && !b.is_empty() {
                branch = Some(b.to_string());
            }

        match val["type"].as_str().unwrap_or("") {
            "assistant" => {
                if let Some(m) = val["message"]["model"].as_str() {
                    model = Some(m.to_string());
                }
                let input = val["message"]["usage"]["input_tokens"]
                    .as_u64()
                    .unwrap_or(0);
                let cache = val["message"]["usage"]["cache_read_input_tokens"]
                    .as_u64()
                    .unwrap_or(0);
                if input + cache > 0 {
                    context_tokens = input + cache;
                }
                if let Some(blocks) = val["message"]["content"].as_array() {
                    for block in blocks {
                        if block["type"] == "tool_use"
                            && let Some(name) = block["name"].as_str() {
                                last_tool = Some(name.to_string());
                            }
                    }
                }
            }
            "user" => {
                let content = &val["message"]["content"];
                if let Some(text) = content.as_str() {
                    if !text.is_empty() {
                        last_prompt = Some(clean_prompt(text.trim()));
                    }
                } else if let Some(blocks) = content.as_array() {
                    for block in blocks {
                        if block["type"] == "text"
                            && let Some(text) = block["text"].as_str() {
                                let text = text.trim();
                                if !text.is_empty() && !text.starts_with("Base directory") {
                                    last_prompt = Some(clean_prompt(text));
                                }
                            }
                    }
                }
            }
            _ => {}
        }
    }

    Some(TranscriptInfo {
        model,
        branch,
        last_prompt,
        last_tool,
        context_tokens,
    })
}
