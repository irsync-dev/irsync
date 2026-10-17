//! Length-prefixed protobuf framing.
//!
//! Each frame is a 4-byte big-endian length followed by that many bytes of a
//! protobuf-encoded message. Nothing else goes on the wire.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest frame body accepted by [`read_msg`] (16 MiB).
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Initial body buffer size; larger bodies grow as their bytes actually arrive.
const INITIAL_BODY_CAPACITY: usize = 64 * 1024;

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
    // Grow the buffer as bytes arrive rather than reserving `len` up front, so a
    // bare length prefix cannot make us allocate MAX_FRAME per stream.
    let mut body = Vec::with_capacity(len.min(INITIAL_BODY_CAPACITY));
    (&mut *r).take(len as u64).read_to_end(&mut body).await?;
    if body.len() != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "end of stream inside frame body",
        ));
    }
    M::decode(body.as_slice())
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use irsync_proto::v1::{Hello, HelloAck};

    async fn read_after<M: prost::Message + Default>(bytes: &[u8]) -> io::Result<Option<M>> {
        let (mut w, mut r) = tokio::io::duplex(1024);
        w.write_all(bytes).await.unwrap();
        drop(w);
        read_msg(&mut r).await
    }

    #[tokio::test]
    async fn max_frame_prefix_then_eof_is_unexpected_eof() {
        let len = u32::try_from(MAX_FRAME).unwrap();
        let err = read_after::<Hello>(&len.to_be_bytes()).await.unwrap_err();
        // UnexpectedEof (not InvalidData): exactly MAX_FRAME passes the length check,
        // and the reader fails on missing bytes instead of pre-allocating the body.
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn zero_length_body_round_trips() {
        let (mut w, mut r) = tokio::io::duplex(1024);
        write_msg(&mut w, &HelloAck::default()).await.unwrap();
        drop(w);
        assert_eq!(
            read_msg::<HelloAck, _>(&mut r).await.unwrap(),
            Some(HelloAck::default())
        );
        assert_eq!(read_msg::<HelloAck, _>(&mut r).await.unwrap(), None);
    }

    #[tokio::test]
    async fn undecodable_body_is_invalid_data() {
        // Field 1 as a length-delimited value claiming 5 bytes, but only 1 follows.
        let err = read_after::<HelloAck>(&[0, 0, 0, 3, 0x0a, 5, 0])
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn write_refuses_oversize_message() {
        let msg = HelloAck {
            session_id: vec![0; MAX_FRAME],
            ..Default::default()
        };
        let mut sink = tokio::io::sink();
        let err = write_msg(&mut sink, &msg).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
