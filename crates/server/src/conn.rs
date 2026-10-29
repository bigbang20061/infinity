//! A client socket that speaks marshalled values over packet framing.

use crate::framing::{inflate, read_frame, write_frame};
use anyhow::{Context, Result};
use marshal::Value;
use std::net::SocketAddr;
use tokio::io::AsyncRead;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::trace;

/// zlib CMF byte used by the client's MachoNet packet compression.
const ZLIB_HEADER: u8 = 0x78;
/// Server diagnostic policy: maximum packet bytes included in decode errors.
const DECODE_ERROR_PREVIEW_BYTES: usize = 64;

pub struct Conn {
    pub stream: TcpStream,
    pub peer: SocketAddr,
}

/// Read one marshalled value; `None` when the peer hung up.
async fn recv_value<R: AsyncRead + Unpin>(r: &mut R, peer: SocketAddr) -> Result<Option<Value>> {
    let Some(mut frame) = read_frame(r).await? else { return Ok(None) };
    // "Packet Compression: MachoNet": the transport zlib-compresses large packets
    // itself, without the framing flag. A marshal stream never starts with 'x'.
    if frame.payload.first() == Some(&ZLIB_HEADER) {
        frame.payload = inflate(&frame.payload).context("inflating machoNet-compressed packet")?;
    }
    let v = marshal::loads(&frame.payload)
        .with_context(|| format!("undecodable packet ({} bytes): {:02x?}", frame.payload.len(), &frame.payload[..frame.payload.len().min(DECODE_ERROR_PREVIEW_BYTES)]))?;
    trace!(%peer, "<< {v:?}");
    Ok(Some(v))
}

impl Conn {
    pub fn new(stream: TcpStream, peer: SocketAddr) -> Self {
        Conn { stream, peer }
    }

    /// Next decoded value, or `None` when the client hung up.
    pub async fn recv(&mut self) -> Result<Option<Value>> {
        recv_value(&mut self.stream, self.peer).await
    }

    pub async fn send(&mut self, v: &Value) -> Result<()> {
        trace!(peer = %self.peer, ">> {v:?}");
        write_frame(&mut self.stream, &marshal::dumps(v)).await?;
        Ok(())
    }

    /// Split into a writer and a background reader feeding a channel, so the
    /// session loop can also wait on server-side pushes.
    pub fn split(self) -> (Writer, mpsc::UnboundedReceiver<Result<Option<Value>>>) {
        let peer = self.peer;
        let (mut rd, wr) = self.stream.into_split();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let r = recv_value(&mut rd, peer).await;
                let stop = !matches!(r, Ok(Some(_)));
                if tx.send(r).is_err() || stop {
                    break;
                }
            }
        });
        (Writer { half: wr, peer }, rx)
    }
}

pub struct Writer {
    half: OwnedWriteHalf,
    pub peer: SocketAddr,
}

impl Writer {
    pub async fn send(&mut self, v: &Value) -> Result<()> {
        trace!(peer = %self.peer, ">> {v:?}");
        write_frame(&mut self.half, &marshal::dumps(v)).await?;
        Ok(())
    }
}
