//! Client half of the IPC protocol: connect, write one request line, read one
//! response line, drop the connection.
//!
//! This lives in core rather than in the CLI because the picker speaks the
//! same protocol. Two copies of the framing rules is one copy too many — a
//! divergence there is exactly the kind of bug that only shows up on
//! multi-line clipboard content.

use crate::ipc::{Request, Response};
use crate::paths;
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

pub fn request(request: Request) -> Result<Response> {
    let path = paths::socket_path();
    let stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "cannot reach clipsterd at {}\n       start it with: systemctl --user start clipsterd",
            path.display()
        )
    })?;

    let mut writer = stream.try_clone()?;
    let mut line = serde_json::to_vec(&request)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()?;

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    if reader.read_line(&mut response)? == 0 {
        bail!("clipsterd closed the connection without responding");
    }
    Ok(serde_json::from_str(&response)?)
}
