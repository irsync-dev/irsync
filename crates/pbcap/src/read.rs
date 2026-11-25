//! Reads the capture files [`pcapng`](crate::pcapng) makes: little-endian pcapng with one
//! exported-PDU interface, one packet at a time.

use std::io::Read;
use std::net::Ipv6Addr;

use crate::pcapng::{TAG_DST_PORT, TAG_END, TAG_IPV6_DST, TAG_IPV6_SRC, TAG_SRC_PORT};
use crate::LINKTYPE_WIRESHARK_UPPER_PDU;

/// The largest block the reader accepts; a larger one is malformed.
/// Only packet blocks are held in memory, and they have a smaller limit.
const MAX_BLOCK: u32 = 16 << 20;

/// The largest Enhanced Packet Block the reader accepts, header and
/// trailer included. The writer never makes one over `MAX_PACKET` bytes of
/// packet, so this leaves room and still caps what one packet can cost.
pub const MAX_PACKET_BLOCK: usize = 1 << 20;

const BLOCK_SECTION: u32 = 0x0a0d_0d0a;
const BLOCK_INTERFACE: u32 = 1;
const BLOCK_PACKET: u32 = 6;
const BYTE_ORDER_MAGIC: u32 = 0x1a2b_3c4d;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the capture is cut off in the middle of a block")]
    Truncated,
    #[error("not an exported-PDU capture: link type {0}, want 252")]
    LinkType(u16),
    #[error("not a capture file this tool reads: {0}")]
    Malformed(&'static str),
}

/// One packet of a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub ts_micros: u64,
    /// The exported-PDU tags before the end tag, as (tag, value with padding).
    pub tags: Vec<(u16, Vec<u8>)>,
    pub body: Vec<u8>,
}

impl Packet {
    fn tag(&self, tag: u16) -> Option<&[u8]> {
        self.tags
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, v)| v.as_slice())
    }

    fn addr(&self, tag: u16) -> Option<Ipv6Addr> {
        let octets: [u8; 16] = self.tag(tag)?.get(..16)?.try_into().ok()?;
        Some(Ipv6Addr::from(octets))
    }

    fn port(&self, tag: u16) -> Option<u32> {
        Some(u32::from_be_bytes(
            self.tag(tag)?.get(..4)?.try_into().ok()?,
        ))
    }

    pub fn src(&self) -> Option<Ipv6Addr> {
        self.addr(TAG_IPV6_SRC)
    }

    pub fn dst(&self) -> Option<Ipv6Addr> {
        self.addr(TAG_IPV6_DST)
    }

    pub fn src_port(&self) -> Option<u32> {
        self.port(TAG_SRC_PORT)
    }

    pub fn dst_port(&self) -> Option<u32> {
        self.port(TAG_DST_PORT)
    }
}

/// Reads one packet at a time. `new` reads up to and including the first
/// Interface Description Block and checks the link type.
pub struct Reader<R: Read> {
    input: R,
    /// A block has been read: the next need not be a section header.
    started: bool,
    done: bool,
}

/// Reads into `buf` until it is full or the input ends; returns the count.
fn fill(input: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match input.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn u16_le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

impl<R: Read> Reader<R> {
    pub fn new(input: R) -> Result<Self, Error> {
        let mut reader = Reader {
            input,
            started: false,
            done: false,
        };
        loop {
            let Some((kind, body)) = reader.block()? else {
                return Err(Error::Truncated);
            };
            match kind {
                BLOCK_SECTION => check_section(&body)?,
                BLOCK_INTERFACE => {
                    check_interface(&body)?;
                    return Ok(reader);
                }
                BLOCK_PACKET => return Err(Error::Malformed("a packet before any interface")),
                _ => {}
            }
        }
    }

    /// The next block as (type, body between the two lengths), or `None`
    /// when the input ends exactly between blocks. Only a packet block's
    /// body is read whole; of any other block the first `KEEP` bytes are
    /// kept, enough for the checks, and the rest is read and dropped.
    fn block(&mut self) -> Result<Option<(u32, Vec<u8>)>, Error> {
        /// Bytes kept of a block that is not a packet.
        const KEEP: u64 = 16;
        let mut head = [0u8; 8];
        match fill(&mut self.input, &mut head)? {
            0 => return Ok(None),
            8 => {}
            _ => return Err(Error::Truncated),
        }
        let kind = u32_le(&head);
        let total = u32_le(&head[4..]);
        if !self.started {
            if kind != BLOCK_SECTION {
                return Err(Error::Malformed(
                    "does not start with a pcapng section header",
                ));
            }
            self.started = true;
        }
        let mut body = Vec::new();
        if kind == BLOCK_SECTION {
            // The byte-order magic says how to read the length, so it is
            // checked first: a big-endian file is named as such.
            let mut magic = [0u8; 4];
            if fill(&mut self.input, &mut magic)? != 4 {
                return Err(Error::Truncated);
            }
            if u32_le(&magic) != BYTE_ORDER_MAGIC {
                return Err(Error::Malformed("not a little-endian section"));
            }
            body.extend_from_slice(&magic);
        }
        if !total.is_multiple_of(4) || !(12..=MAX_BLOCK).contains(&total) {
            return Err(Error::Malformed("a block length out of range"));
        }
        if kind == BLOCK_PACKET && total as usize > MAX_PACKET_BLOCK {
            return Err(Error::Malformed("a packet block larger than 1 MiB"));
        }
        let Some(rest) = (u64::from(total) - 12).checked_sub(body.len() as u64) else {
            return Err(Error::Malformed("a section header that is too short"));
        };
        let keep = if kind == BLOCK_PACKET {
            rest
        } else {
            rest.min(KEEP - body.len() as u64)
        };
        // The buffer grows as bytes arrive, so a block cut short costs no
        // more than what is there.
        let got = (&mut self.input).take(keep).read_to_end(&mut body)? as u64;
        let skipped = std::io::copy(
            &mut (&mut self.input).take(rest - keep),
            &mut std::io::sink(),
        )?;
        if got + skipped != rest {
            return Err(Error::Truncated);
        }
        let mut trailer = [0u8; 4];
        if fill(&mut self.input, &mut trailer)? != 4 {
            return Err(Error::Truncated);
        }
        if u32_le(&trailer) != total {
            return Err(Error::Malformed("a block's two lengths differ"));
        }
        Ok(Some((kind, body)))
    }

    fn next_packet(&mut self) -> Result<Option<Packet>, Error> {
        loop {
            let Some((kind, body)) = self.block()? else {
                return Ok(None);
            };
            match kind {
                BLOCK_SECTION => check_section(&body)?,
                BLOCK_INTERFACE => check_interface(&body)?,
                BLOCK_PACKET => return packet(&body).map(Some),
                _ => {}
            }
        }
    }
}

fn check_section(body: &[u8]) -> Result<(), Error> {
    if body.len() < 16 || u32_le(body) != BYTE_ORDER_MAGIC {
        return Err(Error::Malformed("not a little-endian section"));
    }
    Ok(())
}

fn check_interface(body: &[u8]) -> Result<(), Error> {
    if body.len() < 8 {
        return Err(Error::Malformed("an interface block that is too short"));
    }
    match u16_le(body) {
        LINKTYPE_WIRESHARK_UPPER_PDU => Ok(()),
        other => Err(Error::LinkType(other)),
    }
}

/// An Enhanced Packet Block body: the exported-PDU tags up to the end tag,
/// then the rest of the captured bytes.
fn packet(body: &[u8]) -> Result<Packet, Error> {
    if body.len() < 20 {
        return Err(Error::Malformed("a packet block that is too short"));
    }
    let ts_micros = (u64::from(u32_le(&body[4..])) << 32) | u64::from(u32_le(&body[8..]));
    let captured = u32_le(&body[12..]) as usize;
    let mut pdu = body[20..]
        .get(..captured)
        .ok_or(Error::Malformed("a packet longer than its block"))?;
    let mut tags = Vec::new();
    loop {
        let [t0, t1, l0, l1, rest @ ..] = pdu else {
            return Err(Error::Malformed("a packet without an end tag"));
        };
        let tag = u16::from_be_bytes([*t0, *t1]);
        let len = usize::from(u16::from_be_bytes([*l0, *l1]));
        let value = rest
            .get(..len)
            .ok_or(Error::Malformed("a tag longer than its packet"))?;
        pdu = &rest[len..];
        if tag == TAG_END {
            break;
        }
        tags.push((tag, value.to_vec()));
    }
    Ok(Packet {
        ts_micros,
        tags,
        body: pdu.to_vec(),
    })
}

impl<R: Read> Iterator for Reader<R> {
    type Item = Result<Packet, Error>;

    /// After an error, the next call is `None`.
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_packet() {
            Ok(Some(p)) => Some(Ok(p)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcapng;

    const A: Ipv6Addr = Ipv6Addr::new(0xfd69, 0x7273, 0, 0, 0, 0, 0, 1);
    const B: Ipv6Addr = Ipv6Addr::new(0xfd69, 0x7273, 0, 0, 0, 1, 0, 0);

    fn file(packets: &[(u64, Vec<u8>)]) -> Vec<u8> {
        let mut out = pcapng::section_header();
        out.extend(pcapng::interface(LINKTYPE_WIRESHARK_UPPER_PDU, 0));
        for (ts, pdu) in packets {
            out.extend(pcapng::packet(*ts, pdu));
        }
        out
    }

    #[test]
    fn reads_back_what_the_writer_wrote() {
        let pdu = pcapng::exported_pdu("x", A, B, 7, 9, b"body");
        let got: Vec<_> = Reader::new(&file(&[(5, pdu)])[..])
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(got.len(), 1);
        assert_eq!(
            (got[0].ts_micros, got[0].src(), got[0].dst()),
            (5, Some(A), Some(B))
        );
        assert_eq!((got[0].src_port(), got[0].dst_port()), (Some(7), Some(9)));
        assert_eq!(got[0].body, b"body");
        assert_eq!(got[0].tags[0], (12, b"protobuf".to_vec()));
    }

    #[test]
    fn every_cut_point_is_truncated_or_clean() {
        let pdus = [
            pcapng::exported_pdu("x", A, B, 1, 2, b"one"),
            pcapng::exported_pdu("x", B, A, 3, 4, b"second"),
        ];
        let whole = file(&[(1, pdus[0].clone()), (1 << 40, pdus[1].clone())]);
        let mut boundaries = vec![0];
        for len in [
            pcapng::section_header().len(),
            pcapng::interface(252, 0).len(),
            pcapng::packet(1, &pdus[0]).len(),
            pcapng::packet(1, &pdus[1]).len(),
        ] {
            boundaries.push(boundaries.last().unwrap() + len);
        }
        assert_eq!(*boundaries.last().unwrap(), whole.len());
        let all: Vec<_> = Reader::new(&whole[..])
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(all.len(), 2);
        for cut in 0..=whole.len() {
            let on_boundary = boundaries.contains(&cut);
            let reader = match Reader::new(&whole[..cut]) {
                Ok(r) => r,
                Err(e) => {
                    assert!(matches!(e, Error::Truncated), "cut {cut}: {e}");
                    continue;
                }
            };
            let mut ok = Vec::new();
            let mut err = None;
            for item in reader {
                match item {
                    Ok(p) => ok.push(p),
                    Err(e) => err = Some(e),
                }
            }
            for (a, b) in ok.iter().zip(&all) {
                assert_eq!(
                    (a.ts_micros, &a.body, &a.tags),
                    (b.ts_micros, &b.body, &b.tags)
                );
            }
            match err {
                None => assert!(on_boundary, "cut {cut} inside a block read cleanly"),
                Some(e) => {
                    assert!(!on_boundary, "cut {cut} on a boundary failed: {e}");
                    assert!(matches!(e, Error::Truncated), "cut {cut}: {e}");
                }
            }
        }
    }

    #[test]
    fn empty_input_is_truncated() {
        assert!(matches!(Reader::new(&b""[..]), Err(Error::Truncated)));
    }

    #[test]
    fn other_link_type_is_refused() {
        let mut f = pcapng::section_header();
        f.extend(pcapng::interface(1, 0));
        assert!(matches!(Reader::new(&f[..]), Err(Error::LinkType(1))));
    }

    #[test]
    fn a_second_interface_with_another_link_type_is_refused() {
        let pdu = pcapng::exported_pdu("x", A, B, 1, 2, b"b");
        let mut f = file(&[(1, pdu)]);
        f.extend(pcapng::interface(1, 0));
        let items: Vec<_> = Reader::new(&f[..]).unwrap().collect();
        assert_eq!(items.len(), 2);
        assert!(items[0].is_ok());
        assert!(matches!(items[1], Err(Error::LinkType(1))));
    }

    #[test]
    fn unknown_block_types_are_skipped() {
        let pdu = pcapng::exported_pdu("x", A, B, 1, 2, b"b");
        let mut f = file(&[(1, pdu.clone())]);
        f.extend(block5());
        f.extend(pcapng::packet(2, &pdu));
        let got: Vec<_> = Reader::new(&f[..]).unwrap().map(Result::unwrap).collect();
        assert_eq!(got.iter().map(|p| p.ts_micros).collect::<Vec<_>>(), [1, 2]);
    }

    /// A block of type 5 (name resolution) with 3 bytes of body.
    fn block5() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&5u32.to_le_bytes());
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&[1, 2, 3, 0]);
        b.extend_from_slice(&16u32.to_le_bytes());
        b
    }

    #[test]
    fn absurd_block_length_is_malformed() {
        let mut f = pcapng::section_header();
        f.extend_from_slice(&1u32.to_le_bytes());
        f.extend_from_slice(&0xffff_fff0_u32.to_le_bytes());
        assert!(matches!(Reader::new(&f[..]), Err(Error::Malformed(_))));
        // After the header, the same: the iterator reports it and stops.
        let mut f = file(&[]);
        f.extend_from_slice(&6u32.to_le_bytes());
        f.extend_from_slice(&0xffff_fff0_u32.to_le_bytes());
        let items: Vec<_> = Reader::new(&f[..]).unwrap().collect();
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], Err(Error::Malformed(_))));
    }

    #[test]
    fn big_endian_section_is_malformed() {
        let mut f = Vec::new();
        f.extend_from_slice(&0x0a0d_0d0a_u32.to_be_bytes());
        f.extend_from_slice(&28u32.to_be_bytes());
        f.extend_from_slice(&0x1a2b_3c4d_u32.to_be_bytes());
        f.extend_from_slice(&[0, 1, 0, 0]);
        f.extend_from_slice(&(-1i64).to_be_bytes());
        f.extend_from_slice(&28u32.to_be_bytes());
        let err = Reader::new(&f[..]).err().unwrap();
        assert!(
            matches!(err, Error::Malformed("not a little-endian section")),
            "{err}"
        );
    }

    #[test]
    fn classic_pcap_is_malformed() {
        let mut f = vec![0xd4, 0xc3, 0xb2, 0xa1, 2, 0, 4, 0];
        f.extend_from_slice(&[0; 16]);
        let err = Reader::new(&f[..]).err().unwrap();
        assert!(
            matches!(
                err,
                Error::Malformed("does not start with a pcapng section header")
            ),
            "{err}"
        );
    }

    #[test]
    fn a_file_must_start_with_a_section_header() {
        let f = pcapng::interface(LINKTYPE_WIRESHARK_UPPER_PDU, 0);
        let err = Reader::new(&f[..]).err().unwrap();
        assert!(
            matches!(
                err,
                Error::Malformed("does not start with a pcapng section header")
            ),
            "{err}"
        );
    }

    /// The 8 bytes that start a block: its type and total length.
    fn block_head(kind: u32, total: u32) -> Vec<u8> {
        let mut b = kind.to_le_bytes().to_vec();
        b.extend_from_slice(&total.to_le_bytes());
        b
    }

    #[test]
    fn a_packet_block_over_1_mib_is_malformed_unread() {
        // Only the head: the body is never read, so this is not Truncated.
        let mut f = file(&[]);
        f.extend(block_head(BLOCK_PACKET, MAX_PACKET_BLOCK as u32 + 4));
        let items: Vec<_> = Reader::new(&f[..]).unwrap().collect();
        assert_eq!(items.len(), 1);
        assert!(
            matches!(
                items[0],
                Err(Error::Malformed("a packet block larger than 1 MiB"))
            ),
            "{:?}",
            items[0]
        );
    }

    #[test]
    fn a_packet_block_of_1_mib_is_read() {
        let pdu = pcapng::exported_pdu("x", A, B, 1, 2, b"b");
        let mut block = pcapng::packet(1, &pdu);
        // Grow the block to exactly 1 MiB with padding after the packet.
        let total = MAX_PACKET_BLOCK as u32;
        let pad = MAX_PACKET_BLOCK - block.len();
        let trailer = block.len() - 4;
        block.splice(trailer.., vec![0; pad]);
        block.extend_from_slice(&total.to_le_bytes());
        block[4..8].copy_from_slice(&total.to_le_bytes());
        let mut f = file(&[]);
        f.extend(block);
        let got: Vec<_> = Reader::new(&f[..]).unwrap().map(Result::unwrap).collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body, b"b");
    }

    /// Fails the test on a read into a buffer over 64 KiB: a skipped
    /// block must go through a small buffer, not into memory whole.
    struct Limited<'a> {
        data: &'a [u8],
    }

    impl Read for Limited<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            // A skip reads in small pieces; one large read means the block
            // is being held in memory.
            assert!(buf.len() <= 64 << 10, "a read of {} bytes", buf.len());
            self.data.read(buf)
        }
    }

    #[test]
    fn a_16_mib_unknown_block_cut_short_is_truncated() {
        let mut f = file(&[]);
        let mut block = block_head(5, MAX_BLOCK);
        block.extend_from_slice(&[0; 12]);
        assert_eq!(block.len(), 20);
        f.extend(block);
        let items: Vec<_> = Reader::new(Limited { data: &f[..] }).unwrap().collect();
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], Err(Error::Truncated)), "{:?}", items[0]);
    }

    #[test]
    fn a_2_mib_unknown_block_is_skipped() {
        let pdu = pcapng::exported_pdu("x", A, B, 1, 2, b"b");
        let mut f = file(&[(1, pdu.clone())]);
        let total: u32 = 2 << 20;
        f.extend(block_head(5, total));
        f.extend(vec![7; total as usize - 12]);
        f.extend_from_slice(&total.to_le_bytes());
        f.extend(pcapng::packet(2, &pdu));
        let got: Vec<_> = Reader::new(Limited { data: &f[..] })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(got.iter().map(|p| p.ts_micros).collect::<Vec<_>>(), [1, 2]);
    }
}
