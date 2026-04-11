use std::process::Command;

/// Returns true if Ghostty is the frontmost application.
pub fn has_focus() -> bool {
    let Ok(output) = Command::new("osascript")
        .args(["-e", "tell application \"System Events\" to get name of first application process whose frontmost is true"])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_lowercase()
        .contains("ghostty")
}
