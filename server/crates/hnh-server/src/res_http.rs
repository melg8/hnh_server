//! HTTP resource server: serves `<resname>.res` files so unmodified
//! clients can fetch every resource the session references
//! (`-U http://<host>:<port>/` on the client command line).

use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::info;

pub const RES_PORT: u16 = 1872;

pub async fn spawn(res_dir: PathBuf) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", RES_PORT)).await?;
    info!(port = RES_PORT, dir = %res_dir.display(), "resource http server listening");
    let dir = Arc::new(res_dir);
    loop {
        let (stream, _) = listener.accept().await?;
        let dir = Arc::clone(&dir);
        tokio::spawn(async move {
            if let Err(e) = handle(stream, dir).await {
                tracing::debug!(error = %e, "resource request failed");
            }
        });
    }
}

async fn handle(mut stream: TcpStream, dir: Arc<PathBuf>) -> anyhow::Result<()> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    // Read until end of headers.
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 8192 {
            break;
        }
    }
    let req = String::from_utf8_lossy(&buf);
    let path = req
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .trim_start_matches('/')
        .to_owned();
    // Map "gfx/foo" -> dir/gfx/foo.res; reject path traversal.
    if path.contains("..") || path.contains('\\') {
        stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }
    let file = dir.join(format!("{path}.res"));
    match tokio::fs::read(&file).await {
        Ok(data) => {
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n", data.len());
            stream.write_all(head.as_bytes()).await?;
            stream.write_all(&data).await?;
        }
        Err(_) => {
            stream
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await?;
        }
    }
    stream.flush().await?;
    Ok(())
}
