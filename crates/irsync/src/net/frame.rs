//! Length-prefixed protobuf framing.
//!
//! Each frame is a protobuf varint length followed by that many bytes of a
//! protobuf-encoded message. Nothing else goes on the wire.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest frame body accepted by [`read_msg`] (16 MiB).
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Initial body buffer size; larger bodies grow as their bytes actually arrive.
const INITIAL_BODY_CAPACITY: usize = 64 * 1024;

/// Appends the frame length prefix for a body of `len` bytes: a protobuf
/// varint (at most 4 bytes up to [`MAX_FRAME`]).
pub fn encode_prefix(len: usize, out: &mut Vec<u8>) {
    prost::encoding::encode_varint(len as u64, out);
}

/// Writes `m` as one frame: a varint length, then the encoded body.
pub async fn write_msg<M, W>(w: &mut W, m: &M) -> io::Result<()>
where
    M: prost::Message,
    W: AsyncWrite + Unpin,
{
    let len = m.encoded_len();
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("frame of {len} bytes exceeds {MAX_FRAME}"),
        ));
    }
    let mut buf = Vec::with_capacity(len + 5);
    encode_prefix(len, &mut buf);
    m.encode(&mut buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    w.write_all(&buf).await
}

/// Reads one frame and returns its body, undecoded.
///
/// Returns `Ok(None)` on a clean end of stream before any byte of a new frame.
/// End of stream inside a frame is [`io::ErrorKind::UnexpectedEof`]; an
/// overlong prefix or a length over [`MAX_FRAME`] is
/// [`io::ErrorKind::InvalidData`].
pub async fn read_body<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len: u64 = 0;
    for i in 0..4 {
        let mut b = [0u8; 1];
        if r.read(&mut b).await? == 0 {
            if i == 0 {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "end of stream inside frame length prefix",
            ));
        }
        len |= u64::from(b[0] & 0x7f) << (7 * i);
        if b[0] & 0x80 == 0 {
            return read_sized(r, len).await.map(Some);
        }
    }
    // Four bytes hold 28 bits, enough for MAX_FRAME; a fifth is never needed.
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "frame length prefix too long",
    ))
}

async fn read_sized<R: AsyncRead + Unpin>(r: &mut R, len: u64) -> io::Result<Vec<u8>> {
    if len > MAX_FRAME as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes exceeds {MAX_FRAME}"),
        ));
    }
    let len = len as usize;
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
    Ok(body)
}

/// Reads one frame and decodes it as `M`; see [`read_body`] for the errors.
pub async fn read_msg<M, R>(r: &mut R) -> io::Result<Option<M>>
where
    M: prost::Message + Default,
    R: AsyncRead + Unpin,
{
    match read_body(r).await? {
        None => Ok(None),
        Some(body) => M::decode(body.as_slice())
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irsync_proto::v1::{Hello, HelloAck};
    use prost::Message;

    async fn read_after<M: prost::Message + Default>(bytes: &[u8]) -> io::Result<Option<M>> {
        let (mut w, mut r) = tokio::io::duplex(1024);
        w.write_all(bytes).await.unwrap();
        drop(w);
        read_msg(&mut r).await
    }

    async fn read_after_body(bytes: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let (mut w, mut r) = tokio::io::duplex(1024);
        w.write_all(bytes).await.unwrap();
        drop(w);
        read_body(&mut r).await
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
    async fn write_refuses_oversize_message() {
        let msg = HelloAck {
            session_id: vec![0; MAX_FRAME],
            ..Default::default()
        };
        let mut sink = tokio::io::sink();
        let err = write_msg(&mut sink, &msg).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn prefix_is_protobuf_varint() {
        // A 300-byte body has the prefix AC 02.
        let msg = HelloAck {
            session_id: vec![7; 297],
            ..Default::default()
        };
        assert_eq!(msg.encoded_len(), 300);
        let mut out = Vec::new();
        write_msg(&mut out, &msg).await.unwrap();
        assert_eq!(&out[..2], &[0xAC, 0x02]);
        assert_eq!(out.len(), 302);
    }

    #[tokio::test]
    async fn max_frame_prefix_then_eof_is_unexpected_eof() {
        let mut p = Vec::new();
        encode_prefix(MAX_FRAME, &mut p);
        // UnexpectedEof (not InvalidData): exactly MAX_FRAME passes the length check,
        // and the reader fails on missing bytes instead of pre-allocating the body.
        let err = read_after::<Hello>(&p).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn prefix_over_max_frame_is_invalid_data() {
        let mut p = Vec::new();
        encode_prefix(MAX_FRAME + 1, &mut p);
        let err = read_after::<Hello>(&p).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn eof_inside_prefix_is_unexpected_eof() {
        let err = read_after::<Hello>(&[0x80]).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn overlong_prefix_is_invalid_data() {
        // Six continuation bytes: more than a u32 length can need.
        let err = read_after::<Hello>(&[0x80; 6]).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn undecodable_body_is_invalid_data() {
        // Field 1 as a length-delimited value claiming 5 bytes, but only 1 follows.
        let err = read_after::<HelloAck>(&[3, 0x0a, 5, 0]).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn read_body_returns_raw_bytes() {
        let mut buf = Vec::new();
        write_msg(
            &mut buf,
            &HelloAck {
                resumed: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let body = read_after_body(&buf).await.unwrap().unwrap();
        assert!(HelloAck::decode(&body[..]).unwrap().resumed);
    }
}
