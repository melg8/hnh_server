//! HTTP resource server: serves `<resname>.res` files so unmodified
//! clients can fetch every resource the session references
//! (`-U http://<host>:<port>/` on the client command line).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

pub const RES_PORT: u16 = 1872;

/// Bind the resource HTTP listener. Separate from [`serve`] so startup can
/// fail fast (and loudly) when the port is taken, instead of leaving a
/// half-alive server that only errors on the client side.
pub async fn bind() -> anyhow::Result<TcpListener> {
    Ok(TcpListener::bind(("0.0.0.0", RES_PORT)).await?)
}

/// Serve resources on a pre-bound listener until the process exits.
/// Per-accept errors are transient (Windows can surface WSAECONNRESET when
/// a peer resets before accept), so they log and continue rather than
/// killing the resource server mid-session.
pub async fn serve(listener: TcpListener, res_dir: PathBuf) {
    info!(port = RES_PORT, dir = %res_dir.display(), "resource http server listening");
    let dir = Arc::new(res_dir);
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let dir = Arc::clone(&dir);
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, dir).await {
                        tracing::debug!(error = %e, "resource request failed");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "resource accept error");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle(mut stream: TcpStream, dir: Arc<PathBuf>) -> anyhow::Result<()> {
    let peer = stream.peer_addr().ok();
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
        warn!(?peer, path = %path, "res 403 (path traversal)");
        stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }
    let file = dir.join(format!("{path}.res"));
    match tokio::fs::read(&file).await {
        Ok(data) => {
            info!(?peer, path = %path, bytes = data.len(), "res 200");
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n", data.len());
            stream.write_all(head.as_bytes()).await?;
            stream.write_all(&data).await?;
        }
        Err(_) => {
            warn!(?peer, path = %path, "res 404 (missing resource)");
            stream
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await?;
        }
    }
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive one request through `handle` over a real loopback pair and
    /// return the HTTP status code plus the response body.
    async fn request(dir: &std::path::Path, target: &str) -> (u16, Vec<u8>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = TcpListener::from_std(listener).unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let req = format!("GET /{target} HTTP/1.1\r\nHost: test\r\n\r\n");
        client.write_all(req.as_bytes()).await.unwrap();
        let dir = Arc::new(dir.to_path_buf());
        let (resp, body) = tokio::join!(handle(server, dir), async {
            let mut buf = Vec::new();
            client.read_to_end(&mut buf).await.unwrap();
            buf
        });
        resp.unwrap();
        let text = String::from_utf8_lossy(&body).into_owned();
        let status: u16 = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let body = body
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| body[i + 4..].to_vec())
            .unwrap_or_default();
        (status, body)
    }

    fn temp_resdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hnh-res-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("gfx/hud")).unwrap();
        std::fs::write(dir.join("gfx/hud/fbtn.res"), b"FAKE-RES-DATA").unwrap();
        dir
    }

    #[tokio::test]
    async fn serves_existing_resource() {
        let dir = temp_resdir("ok");
        let (status, body) = request(&dir, "gfx/hud/fbtn").await;
        assert_eq!(status, 200);
        assert_eq!(body, b"FAKE-RES-DATA");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn missing_resource_is_404() {
        let dir = temp_resdir("miss");
        let (status, _) = request(&dir, "gfx/hud/nope").await;
        assert_eq!(status, 404);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn path_traversal_is_rejected() {
        let dir = temp_resdir("trav");
        let (status, _) = request(&dir, "../Cargo.toml").await;
        assert_eq!(status, 403);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
