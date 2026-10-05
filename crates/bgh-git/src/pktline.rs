//! Git pkt-line framing.
//!
//! A pkt-line is a 4-hex-digit length (including the 4 bytes) followed by
//! the payload; `0000` is a flush packet, `0001` a delimiter, `0002`
//! response-end (protocol v2).

use bytes::{BufMut, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};

pub const FLUSH: &[u8] = b"0000";
/// Maximum pkt-line length (65520) per the protocol.
pub const MAX_PKT_LEN: usize = 65520;

/// Encode one pkt-line.
pub fn encode(payload: &[u8]) -> Bytes {
    let mut b = BytesMut::with_capacity(payload.len() + 4);
    put(&mut b, payload);
    b.freeze()
}

/// Append one pkt-line to `buf`.
pub fn put(buf: &mut BytesMut, payload: &[u8]) {
    buf.put_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    buf.put_slice(payload);
}

/// Append a side-band packet on `channel` (1 = data, 2 = progress, 3 = error),
/// splitting large payloads.
pub fn put_sideband(buf: &mut BytesMut, channel: u8, payload: &[u8]) {
    for chunk in payload.chunks(MAX_PKT_LEN - 5) {
        let mut p = Vec::with_capacity(chunk.len() + 1);
        p.push(channel);
        p.extend_from_slice(chunk);
        put(buf, &p);
    }
}

/// A parsed packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Flush,
    Delim,
    ResponseEnd,
    Data(Vec<u8>),
}

/// Read one packet. Returns the packet and the raw bytes consumed.
pub async fn read_packet<R: AsyncRead + Unpin + ?Sized>(
    r: &mut R,
    raw: &mut Vec<u8>,
) -> std::io::Result<Packet> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    raw.extend_from_slice(&len_buf);
    let len_str = std::str::from_utf8(&len_buf)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad pkt-line length"))?;
    let len = usize::from_str_radix(len_str, 16)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad pkt-line length"))?;
    match len {
        0 => Ok(Packet::Flush),
        1 => Ok(Packet::Delim),
        2 => Ok(Packet::ResponseEnd),
        3 => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bad pkt-line length",
        )),
        n if n > MAX_PKT_LEN => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "pkt-line too long",
        )),
        n => {
            let mut data = vec![0u8; n - 4];
            r.read_exact(&mut data).await?;
            raw.extend_from_slice(&data);
            Ok(Packet::Data(data))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip() {
        let mut buf = BytesMut::new();
        put(&mut buf, b"hello\n");
        buf.put_slice(FLUSH);
        assert_eq!(&buf[..], b"000ahello\n0000");
        let mut r = &buf[..];
        let mut raw = Vec::new();
        assert_eq!(
            read_packet(&mut r, &mut raw).await.unwrap(),
            Packet::Data(b"hello\n".to_vec())
        );
        assert_eq!(read_packet(&mut r, &mut raw).await.unwrap(), Packet::Flush);
        assert_eq!(raw, buf.to_vec());
    }

    #[test]
    fn sideband() {
        let mut buf = BytesMut::new();
        put_sideband(&mut buf, 2, b"hi");
        assert_eq!(&buf[..], b"0007\x02hi");
    }
}
