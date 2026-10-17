//! Length-prefixed protobuf framing.
//!
//! Each frame is a 4-byte big-endian length followed by that many bytes of a
//! protobuf-encoded message. Nothing else goes on the wire.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest frame body accepted by [`read_msg`] (16 MiB).
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Writes `m` as one frame: a big-endian `u32` length, then the encoded body.
pub async fn write_msg<M, W>(w: &mut W, m: &M) -> io::Result<()>
where
    M: prost::Message,
    W: AsyncWrite + Unpin,
{
    let body = m.encode_to_vec();
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("frame of {} bytes exceeds {MAX_FRAME}", body.len()),
        ));
    }
    let len = body.len() as u32;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&body).await
}

/// Reads one frame and decodes it as `M`.
///
/// Returns `Ok(None)` on a clean end of stream before any byte of a new frame.
/// End of stream inside a frame is [`io::ErrorKind::UnexpectedEof`]; an
/// oversize length or undecodable body is [`io::ErrorKind::InvalidData`].
pub async fn read_msg<M, R>(r: &mut R) -> io::Result<Option<M>>
where
    M: prost::Message + Default,
    R: AsyncRead + Unpin,
{
    let mut prefix = [0u8; 4];
    let mut filled = 0;
    while filled < prefix.len() {
        let n = r.read(&mut prefix[filled..]).await?;
        if n == 0 {
            if filled == 0 {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "end of stream inside frame length prefix",
            ));
        }
        filled += n;
    }
    let len = u32::from_be_bytes(prefix) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes exceeds {MAX_FRAME}"),
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    M::decode(body.as_slice())
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
