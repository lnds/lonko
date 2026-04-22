mod agents;
mod app;
mod config;
mod control;
mod event;
mod focus;
mod install;
mod install_remote;
mod respond;
mod sources;
mod state;
mod ui;
mod new_agent;
mod worktree;

use color_eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("--install-hooks") {
        println!("Installing lonko hooks into ~/.claude/settings.json...\n");
        return install::run().map_err(|e| color_eyre::eyre::eyre!(e));
    }
    if args.get(1).map(|s| s.as_str()) == Some("focus") {
        let n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        return focus::run(n).map_err(|e| color_eyre::eyre::eyre!(e));
    }
    if args.get(1).map(|s| s.as_str()) == Some("respond") {
        let key = args.get(2).map(|s| s.as_str()).unwrap_or("");
        if !matches!(key, "y" | "n" | "w") {
            eprintln!("usage: lonko respond <y|n|w>");
            std::process::exit(1);
        }
        return respond::run(key).map_err(|e| color_eyre::eyre::eyre!(e));
    }
    if args.get(1).map(|s| s.as_str()) == Some("install-remote") {
        let host = args.get(2).map(|s| s.as_str()).unwrap_or("");
        if host.is_empty() {
            eprintln!("usage: lonko install-remote <host>");
            std::process::exit(1);
        }
        return install_remote::run(host).map_err(|e| color_eyre::eyre::eyre!(e));
    }
    if args.get(1).map(|s| s.as_str()) == Some("worktree") {
        let cwd = args.get(2).map(|s| s.as_str()).unwrap_or("");
        let branch = args.get(3).map(|s| s.as_str()).unwrap_or("");
        if cwd.is_empty() || branch.is_empty() {
            eprintln!("usage: lonko worktree <cwd> <branch>");
            std::process::exit(1);
        }
        return worktree::run(cwd, branch).map_err(|e| color_eyre::eyre::eyre!(e));
    }

    // Parse --tab <agents|sessions> for initial tab selection
    let initial_tab = args.windows(2)
        .find(|w| w[0] == "--tab")
        .and_then(|w| match w[1].as_str() {
            "agents" => Some(state::Tab::Agents),
            "sessions" => Some(state::Tab::Sessions),
            _ => None,
        });

    // File-backed tracing so debug lines from the bridge, hook handler,
    // etc. go somewhere observable. Respects `LONKO_LOG` (e.g.
    // `LONKO_LOG=debug`) and defaults to `info`. The log file is kept in
    // the OS cache dir to avoid polluting $HOME. Creation failures are
    // swallowed — a lonko without logs still works.
    let log_path = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("lonko.log");
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use tracing_subscriber::{fmt, EnvFilter};
        let filter = EnvFilter::try_from_env("LONKO_LOG")
            .unwrap_or_else(|_| EnvFilter::new("info"));
        let _ = fmt()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .with_env_filter(filter)
            .try_init();
    }

    let mut terminal = ratatui::init();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableFocusChange,
    )?;
    let mut app = app::App::new();
    if let Some(tab) = initial_tab {
        app.state.active_tab = tab;
    }
    let result = app.run(&mut terminal).await;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableFocusChange,
    )?;
    ratatui::restore();
    result
}
