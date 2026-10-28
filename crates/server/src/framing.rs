//! Packet socket framing used by `sendpacket` / `recvpacketoob` in the
//! client's blue.dll (stacklessio).
//!
//! Each packet is a little-endian u32 header followed by the body. The low 28
//! bits are the body length; the high bits are flags (carbonengine io
//! `protocol.h`). Little-endian was confirmed by disassembling the client's
//! parser, which loads the header with a plain `mov` and masks `0x0FFFFFFF`.

use std::io::{self, Read};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const SIZE_MASK: u32 = 0x0FFF_FFFF;
/// Body starts with a u32 OOB length, then OOB bytes, then the payload.
const FLAG_OOB: u32 = 1 << 28;
const FLAG_ZLIB: u32 = 1 << 29;
const FLAG_SNAPPY: u32 = 1 << 30;

pub const MAX_PACKET: usize = 10 * 1024 * 1024; // SocketTransportFactory.MaxPacketSize

pub struct Frame {
    pub payload: Vec<u8>,
    pub oob: Option<Vec<u8>>,
}

/// Read one packet; `Ok(None)` on clean EOF at a packet boundary.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Frame>> {
    let mut hdr = [0u8; 4];
    match r.read_exact(&mut hdr).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let header = u32::from_le_bytes(hdr);
    let len = (header & SIZE_MASK) as usize;
    if len > MAX_PACKET {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("packet too large: {len} bytes")));
    }
    if header & FLAG_SNAPPY != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snappy-compressed packet"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;

    let (oob, mut payload) = if header & FLAG_OOB != 0 && body.len() >= 4 {
        let n = u32::from_le_bytes(body[..4].try_into().unwrap()) as usize;
        if 4 + n > body.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "OOB length exceeds packet"));
        }
        (Some(body[4..4 + n].to_vec()), body[4 + n..].to_vec())
    } else {
        (None, body)
    };
    if header & FLAG_ZLIB != 0 {
        let mut out = Vec::with_capacity(payload.len() * 4);
        flate2::read::ZlibDecoder::new(&payload[..]).read_to_end(&mut out)?;
        payload = out;
    }
    Ok(Some(Frame { payload, oob }))
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    if payload.len() > SIZE_MASK as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "packet too large"));
    }
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf).await
}
