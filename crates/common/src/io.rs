//! Framed paket IO: her mesaj u32-LE uzunluk + bincode.
//! Herhangi bir AsyncRead/AsyncWrite üzerinde çalışır (TCP, TLS, dial-back).

use anyhow::Result;
use anyhow::Context as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{decode, encode, rv_decode, rv_encode, Packet, RvMsg};

pub const MAX_MSG: usize = 24_000_000;
pub const MAX_CONTROL_MSG: usize = 1_000_000;

/// Ham framed blob: [uzunluk:u32][bayt] (relay kopyalama için, decode etmeden).
pub async fn read_blob<R>(r: &mut R) -> Result<Vec<u8>>
where
    R: AsyncReadExt + Unpin,
{
    let len = r.read_u32().await? as usize;
    if len > MAX_MSG {
        anyhow::bail!("blob çok büyük: {len}");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_blob<W>(w: &mut W, blob: &[u8]) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    if blob.len() > MAX_MSG { anyhow::bail!("blob çok büyük: {}", blob.len()); }
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
        anyhow::bail!("paket çok büyük: {len} (sınır {max_len})");
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
        anyhow::bail!("paket çok büyük: {}", buf.len());
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
        anyhow::bail!("rv paket çok büyük: {len}");
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
        anyhow::bail!("rv paket çok büyük: {}", buf.len());
    }
    w.write_u32(buf.len() as u32).await?;
    w.write_all(&buf).await?;
    Ok(())
}

/// Web-oturum dial-back hattı: [tür:1B][uzunluk:u32][bayt].
/// tür 0 = binary (video), 1 = text (JSON). Tarayıcı WS karşılıkları.
pub async fn write_kmsg<W>(w: &mut W, kind: u8, payload: &[u8]) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    if payload.len() > MAX_MSG { anyhow::bail!("kmsg çok büyük: {}", payload.len()); }
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
    // kmsg hem küçük JSON kontrollerini hem de video karelerini taşır. Tür bazlı
    // daha dar JSON sınırı uç noktalarda ayrıca uygulanır.
    if len > MAX_MSG {
        anyhow::bail!("kmsg çok büyük: {len}");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok((kind, buf))
}

/// Önceden etiketlenmiş [tür, ...bayt] yazımı.
pub async fn write_kmsg_raw<W>(w: &mut W, tagged: &[u8]) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let (kind, payload) = tagged.split_first().context("boş kmsg")?;
    write_kmsg(w, *kind, payload).await
}
