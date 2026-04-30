use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::agents::claude;

#[derive(Debug)]
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

/// Return the path of the most recently modified `.jsonl` transcript for
/// `cwd`, or `None` if no transcripts exist. Used to recover the current
/// session id when the lifecycle file (`~/.claude/sessions/<pid>.json`) is
/// stale — Claude Code rewrites the transcript on every `/clear`, but does
/// not update the lifecycle file's `sessionId`, so the lifecycle file can
/// point at a sessionId that was superseded days ago.
pub fn most_recent_transcript_session(cwd: &str) -> Option<(PathBuf, String)> {
    let dir = claude::transcript_path(cwd, "").parent()?.to_path_buf();
    let entries = std::fs::read_dir(&dir).ok()?;

    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best = Some((mtime, path));
        }
    }

    let (_, path) = best?;
    let session_id = path.file_stem()?.to_str()?.to_string();
    Some((path, session_id))
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

/// `true` when a user-role text block is a runtime-injected message rather
/// than a real prompt typed by the user. Claude Code slips these into the
/// conversation as `type: "text"` user blocks and also ships them as the
/// `prompt` field of `UserPromptSubmit` hooks when the runtime re-fires a
/// scheduled `/loop`. Without filtering they end up shown as the
/// "last prompt" on the agent card.
pub(crate) fn is_system_injected(text: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "<task-notification>",
        "<system-reminder>",
        "<user-prompt-submit-hook>",
        "<bash-",                     // <bash-input>, <bash-stdout>, <bash-stderr>
        "<local-command-",            // <local-command-stdout>, <local-command-stderr>
        "<<autonomous-loop-dynamic",  // /loop dynamic self-pacing sentinel
        "<<autonomous-loop>",         // /loop CronCreate sentinel
        "Base directory",             // initial system message from Claude Code
        "Caveat: The messages below were generated",
    ];
    PREFIXES.iter().any(|p| text.starts_with(p))
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
            && !b.is_empty()
            && b != "HEAD" {
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
                    let text = text.trim();
                    if !text.is_empty() && !is_system_injected(text) {
                        last_prompt = Some(clean_prompt(text));
                    }
                } else if let Some(blocks) = content.as_array() {
                    for block in blocks {
                        if block["type"] == "text"
                            && let Some(text) = block["text"].as_str() {
                                let text = text.trim();
                                if !text.is_empty() && !is_system_injected(text) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_system_injected_flags_task_notification() {
        assert!(is_system_injected("<task-notification>\n<task-id>abc</task-id></task-notification>"));
    }

    #[test]
    fn is_system_injected_flags_system_reminder() {
        assert!(is_system_injected("<system-reminder>something</system-reminder>"));
    }

    #[test]
    fn is_system_injected_flags_bash_io_blocks() {
        assert!(is_system_injected("<bash-input>ls -la</bash-input>"));
        assert!(is_system_injected("<bash-stdout>total 0</bash-stdout>"));
    }

    #[test]
    fn is_system_injected_flags_hook_envelope() {
        assert!(is_system_injected("<user-prompt-submit-hook>...</user-prompt-submit-hook>"));
    }

    #[test]
    fn is_system_injected_flags_base_directory() {
        assert!(is_system_injected("Base directory: /tmp/x"));
    }

    #[test]
    fn is_system_injected_flags_autonomous_loop_sentinels() {
        assert!(is_system_injected("<<autonomous-loop-dynamic>"));
        assert!(is_system_injected("<<autonomous-loop-dynamic>>"));
        assert!(is_system_injected("<<autonomous-loop>"));
        assert!(is_system_injected("<<autonomous-loop>>"));
    }

    #[test]
    fn is_system_injected_passes_real_prompt() {
        assert!(!is_system_injected("hacer X"));
        assert!(!is_system_injected("<command-name>/skill</command-name><command-message>arg</command-message>"));
    }

    #[test]
    fn clean_prompt_extracts_slash_command() {
        let raw = "<command-message>arg</command-message><command-name>/skill</command-name>";
        assert_eq!(clean_prompt(raw), "/skill arg");
    }

    #[test]
    fn clean_prompt_strips_tags_for_plain_text() {
        assert_eq!(clean_prompt("<foo>bar</foo>baz"), "barbaz");
    }
}
