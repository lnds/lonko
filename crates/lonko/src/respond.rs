// Send a permission response to a running lonko instance via the Unix socket.

use std::io::Write;
use std::os::unix::net::UnixStream;

use anyhow::Result;

use crate::sources::hooks;

pub fn run(key: &str) -> Result<()> {
    let path = hooks::socket_path();
    let mut stream = UnixStream::connect(&path)
        .map_err(|e| anyhow::anyhow!("cannot connect to lonko at {}: {e}", path.display()))?;
    writeln!(stream, "permission {key}")?;
    Ok(())
}
