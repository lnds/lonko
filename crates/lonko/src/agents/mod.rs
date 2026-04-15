//! Agent-specific constants and helpers.
//!
//! Today Lonko only supports Claude Code, but every Claude-specific path,
//! process name, and quirk lives here so a second agent implementation
//! (Codex, Gemini, Aider, ...) can sit next to `claude` without rippling
//! changes across the rest of the codebase.

pub mod claude;
