//! Image server: the client's `imageserverurl` (evePhotosvc RemoteImageCacher)
//! fetches `<Character|Corporation|Alliance|Type|Render>/<id>_<size>.<jpg|png>`.
//! Images live under a data directory; `PUT` stores one (used to publish the
//! portraits the client renders locally at character creation). It also serves
//! the content CDN under `/content/` (crate::cdn).
//!
//! A size that isn't stored is served from the nearest stored size of the same image.
//! Images of things that exist on CCP's servers under the same IDs (NPC characters such
//! as agents, NPC corporations, factions, types) are fetched once from the upstream image
//! server (IMAGE_UPSTREAM) and kept. Player characters, corporations and alliances are
//! never fetched: their IDs are this server's own.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

const MAX_UPLOAD: usize = 8 * 1024 * 1024;
const KINDS: [&str; 5] = ["Character", "Corporation", "Alliance", "Type", "Render"];
/// Sizes the image server offers.
const SIZES: [u32; 6] = [32, 64, 128, 256, 512, 1024];
/// Character and corporation IDs from here on are players' (this server's own).
const FIRST_PLAYER_ID: u64 = 90_000_000;

/// The upstream image server, if any.
#[derive(Clone)]
pub struct Upstream(Option<Arc<(String, reqwest::Client)>>);

impl Upstream {
    pub fn new(base: &str) -> Upstream {
        let base = base.trim_end_matches('/');
        if base.is_empty() {
            return Upstream(None);
        }
        let client = reqwest::Client::builder().user_agent("infinity-emulator image cache").timeout(std::time::Duration::from_secs(10)).build();
        Upstream(client.ok().map(|c| Arc::new((base.to_string(), c))))
    }

    /// `<kind>/<id>_<size>.<ext>` on images.evetech.net, for images that are the same there.
    fn url(&self, kind: &str, id: u64, size: u32) -> Option<String> {
        let (base, _) = self.0.as_deref()?;
        let size = SIZES.into_iter().find(|s| *s >= size).unwrap_or(1024);
        Some(match kind {
            "Character" if id < FIRST_PLAYER_ID => format!("{base}/characters/{id}/portrait?size={size}"),
            "Corporation" if id < FIRST_PLAYER_ID => format!("{base}/corporations/{id}/logo?size={size}"),
            "Alliance" if id < FIRST_PLAYER_ID => format!("{base}/alliances/{id}/logo?size={size}"),
            "Type" => format!("{base}/types/{id}/icon?size={}", size.min(64)),
            "Render" => format!("{base}/types/{id}/render?size={size}"),
            _ => return None,
        })
    }

    async fn fetch(&self, kind: &str, id: u64, size: u32) -> Option<Vec<u8>> {
        let url = self.url(kind, id, size)?;
        let (_, client) = self.0.as_deref()?;
        let response = client.get(&url).send().await.ok()?;
        if !response.status().is_success() {
            warn!("images: upstream {url} -> {}", response.status());
            return None;
        }
        let body = response.bytes().await.ok()?;
        info!("images: fetched {url} ({} bytes)", body.len());
        Some(body.to_vec())
    }
}

/// Serve on `bind`; `name` labels this listener's request log lines ("images", "cdn").
pub async fn serve(name: &'static str, bind: String, root: PathBuf, upstream: Upstream) -> Result<()> {
    for k in KINDS {
        tokio::fs::create_dir_all(root.join(k)).await?;
    }
    let listener = TcpListener::bind(&bind).await?;
    info!("{name}: listening on {bind}, storing images in {}", root.display());
    loop {
        let (stream, peer) = listener.accept().await?;
        let (root, upstream) = (root.clone(), upstream.clone());
        tokio::spawn(async move {
            if let Err(e) = handle(stream, &root, name, peer, &upstream).await {
                warn!("{name}: request from {peer} failed: {e:#}");
            }
        });
    }
}

/// One request, for the log line written with its response.
struct Req<'a> {
    name: &'static str,
    peer: std::net::SocketAddr,
    method: &'a str,
    path: &'a str,
}

/// `/Character/90000001_512.jpg` -> `<root>/Character/90000001_512.jpg`, rejecting anything else.
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let path = path.split('?').next()?.trim_start_matches('/');
    let (kind, file) = path.split_once('/')?;
    if !KINDS.contains(&kind) {
        return None;
    }
    let (stem, ext) = file.rsplit_once('.')?;
    let (id, size) = stem.split_once('_')?;
    if !matches!(ext, "jpg" | "png") || id.parse::<u64>().is_err() || size.parse::<u32>().is_err() {
        return None;
    }
    Some(root.join(kind).join(file))
}

async fn respond(w: &mut TcpStream, req: &Req<'_>, status: &str, ctype: &str, body: &[u8], head_only: bool) -> Result<()> {
    if status.starts_with('2') {
        info!("{}: {} {} {} -> {status} ({} bytes)", req.name, req.peer, req.method, req.path, body.len());
    } else {
        warn!("{}: {} {} {} -> {status}", req.name, req.peer, req.method, req.path);
    }
    let hdr = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    w.write_all(hdr.as_bytes()).await?;
    if !head_only {
        w.write_all(body).await?;
    }
    Ok(())
}

/// The image at `file`, else the nearest stored size of it (larger first), else fetched
/// from upstream and stored.
async fn find(file: &Path, upstream: &Upstream) -> Option<Vec<u8>> {
    if let Ok(data) = tokio::fs::read(file).await {
        return Some(data);
    }
    let kind = file.parent()?.file_name()?.to_str()?;
    let stem = file.file_stem()?.to_str()?;
    let ext = file.extension()?.to_str()?;
    let (id, size) = stem.split_once('_')?;
    let (id, size): (u64, u32) = (id.parse().ok()?, size.parse().ok()?);
    let mut sizes: Vec<u32> = SIZES.to_vec();
    sizes.sort_by_key(|s| (*s < size, s.abs_diff(size)));
    for s in sizes {
        for e in [ext, if ext == "jpg" { "png" } else { "jpg" }] {
            if let Ok(data) = tokio::fs::read(file.with_file_name(format!("{id}_{s}.{e}"))).await {
                return Some(data);
            }
        }
    }
    let data = upstream.fetch(kind, id, size).await?;
    if let Err(e) = tokio::fs::write(file, &data).await {
        warn!("images: can't store {}: {e}", file.display());
    }
    Some(data)
}

async fn handle(mut stream: TcpStream, root: &Path, name: &'static str, peer: std::net::SocketAddr, upstream: &Upstream) -> Result<()> {
    let mut reader = BufReader::new(&mut stream);
    // Read raw bytes: a request line or header may not be UTF-8.
    let mut raw = Vec::new();
    reader.read_until(b'\n', &mut raw).await?;
    let line = String::from_utf8_lossy(&raw).into_owned();
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/").to_string());
    let mut content_length = 0usize;
    loop {
        let mut raw = Vec::new();
        if reader.read_until(b'\n', &mut raw).await? == 0 || raw == b"\r\n" || raw == b"\n" {
            break;
        }
        let h = String::from_utf8_lossy(&raw);
        if let Some((k, v)) = h.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let upload = matches!(method.as_str(), "PUT" | "POST");
    let mut body = Vec::new();
    if upload && content_length > 0 && content_length <= MAX_UPLOAD {
        body = vec![0u8; content_length];
        reader.read_exact(&mut body).await?;
    }
    drop(reader);
    let req = Req { name, peer, method: &method, path: &path };
    if upload && (content_length == 0 || content_length > MAX_UPLOAD) {
        warn!("{name}: {peer} {method} {path}: body of {content_length} bytes rejected (limit {MAX_UPLOAD})");
    }
    if let Some(name) = path.strip_prefix("/content/") {
        return match crate::cdn::content(name.split('?').next().unwrap_or("")) {
            Some((data, _)) if matches!(method.as_str(), "GET" | "HEAD") => {
                respond(&mut stream, &req, "200 OK", "application/octet-stream", data, method == "HEAD").await
            }
            _ => respond(&mut stream, &req, "404 Not Found", "text/plain", b"not found", method == "HEAD").await,
        };
    }
    let Some(file) = resolve(root, &path) else {
        return respond(&mut stream, &req, "404 Not Found", "text/plain", b"not found", method == "HEAD").await;
    };
    match method.as_str() {
        "GET" | "HEAD" => match find(&file, upstream).await {
            Some(data) => {
                // By content: a fallback may be the other format.
                let ctype = if data.starts_with(b"\x89PNG") { "image/png" } else { "image/jpeg" };
                respond(&mut stream, &req, "200 OK", ctype, &data, method == "HEAD").await
            }
            None => respond(&mut stream, &req, "404 Not Found", "text/plain", b"not found", method == "HEAD").await,
        },
        "PUT" | "POST" => {
            if body.is_empty() {
                return respond(&mut stream, &req, "413 Payload Too Large", "text/plain", b"bad size", false).await;
            }
            tokio::fs::write(&file, &body).await?;
            info!("{name}: stored {} ({} bytes) from {peer}", file.display(), body.len());
            respond(&mut stream, &req, "201 Created", "text/plain", b"stored", false).await
        }
        _ => {
            respond(&mut stream, &req, "405 Method Not Allowed", "text/plain", b"method not allowed", false).await
        }
    }
}
