//! Framed packet IO: each message is a u32 length prefix + bincode.
//! Works over any AsyncRead/AsyncWrite (TCP, TLS, dial-back).

use anyhow::Result;
use anyhow::Context as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{decode, encode, rv_decode, rv_encode, Packet, RvMsg};

pub const MAX_MSG: usize = 24_000_000;
pub const MAX_CONTROL_MSG: usize = 1_000_000;

/// Raw framed blob: [length:u32][bytes] (for relay copying, without decoding).
pub async fn read_blob<R>(r: &mut R) -> Result<Vec<u8>>
where
    R: AsyncReadExt + Unpin,
{
    let len = r.read_u32().await? as usize;
    if len > MAX_MSG {
        anyhow::bail!("blob too large: {len}");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_blob<W>(w: &mut W, blob: &[u8]) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    if blob.len() > MAX_MSG { anyhow::bail!("blob too large: {}", blob.len()); }
    w.write_u32(blob.len() as u32).await?;
    w.write_all(blob).await?;
    Ok(())
}

pub async fn read_packet_limited<R>(r: &mut R, max_len: usize) -> Result<Packet>
where
    R: AsyncReadExt + Unpin,
{
    let max_len = max_len.min(MAX_MSG);
    let len = r.read_u32().await? as usize;
    if len > max_len {
        anyhow::bail!("packet too large: {len} (limit {max_len})");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    decode(&buf)
}

pub async fn read_packet<R>(r: &mut R) -> Result<Packet>
where
    R: AsyncReadExt + Unpin,
{
    read_packet_limited(r, MAX_MSG).await
}

pub async fn write_packet<W>(w: &mut W, p: &Packet) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let buf = encode(p)?;
    if buf.len() > MAX_MSG {
        anyhow::bail!("packet too large: {}", buf.len());
    }
    w.write_u32(buf.len() as u32).await?;
    w.write_all(&buf).await?;
    Ok(())
}

pub async fn read_rv<R>(r: &mut R) -> Result<RvMsg>
where
    R: AsyncReadExt + Unpin,
{
    let len = r.read_u32().await? as usize;
    if len > MAX_CONTROL_MSG {
        anyhow::bail!("rv packet too large: {len}");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    rv_decode(&buf)
}

pub async fn write_rv<W>(w: &mut W, m: &RvMsg) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let buf = rv_encode(m)?;
    if buf.len() > MAX_CONTROL_MSG {
        anyhow::bail!("rv packet too large: {}", buf.len());
    }
    w.write_u32(buf.len() as u32).await?;
    w.write_all(&buf).await?;
    Ok(())
}

/// Web-session dial-back line: [kind:1B][length:u32][bytes].
/// kind 0 = binary (video), 1 = text (JSON). Map to browser WS message types.
pub async fn write_kmsg<W>(w: &mut W, kind: u8, payload: &[u8]) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    if payload.len() > MAX_MSG { anyhow::bail!("kmsg too large: {}", payload.len()); }
    w.write_u8(kind).await?;
    w.write_u32(payload.len() as u32).await?;
    w.write_all(payload).await?;
    Ok(())
}

pub async fn read_kmsg<R>(r: &mut R) -> Result<(u8, Vec<u8>)>
where
    R: AsyncReadExt + Unpin,
{
    let kind = r.read_u8().await?;
    let len = r.read_u32().await? as usize;
    // kmsg carries both small JSON control messages and video frames. A tighter
    // per-kind JSON limit is enforced separately at the endpoints.
    if len > MAX_MSG {
        anyhow::bail!("kmsg too large: {len}");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok((kind, buf))
}

/// Write a pre-tagged [kind, ...bytes] message.
pub async fn write_kmsg_raw<W>(w: &mut W, tagged: &[u8]) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let (kind, payload) = tagged.split_first().context("empty kmsg")?;
    write_kmsg(w, *kind, payload).await
}
