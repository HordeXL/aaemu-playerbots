// Vendored from AAEmu-1.2-Rust rust/crates/aaemu-commons/src/net.rs (GPL-3.0-or-later).
//! TCP framing shared by every AAEmu protocol (replaces NetCoreServer + the hand-rolled
//! reassembly in each `*ProtocolHandler.OnReceive`).
//!
//! Every frame on the wire is `[u16 LE length][payload]`, where `length` counts the payload only.
//!
//! Unlike the server's codec, the decoder yields [`Bytes`]: the payload is a zero-copy view of the
//! read buffer, so a frame costs no allocation or copy. Frames share the reader's buffer; a
//! consumer that keeps a frame (or a slice of it) for long pins that buffer, so parse and drop.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

/// Splits a byte stream into length-prefixed frames. Yields the payload with the length removed.
#[derive(Debug, Default, Clone, Copy)]
pub struct FrameCodec;

impl Decoder for FrameCodec {
    type Item = Bytes;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 2 {
            return Ok(None);
        }
        let len = usize::from(u16::from_le_bytes([src[0], src[1]]));
        if src.len() < 2 + len {
            src.reserve(2 + len - src.len());
            return Ok(None);
        }
        src.advance(2);
        Ok(Some(src.split_to(len).freeze()))
    }
}

impl<T: AsRef<[u8]>> Encoder<T> for FrameCodec {
    type Error = std::io::Error;

    /// Prepends the `u16` length to `payload` (a `Vec<u8>`, or a shared `Bytes` frame).
    fn encode(&mut self, payload: T, dst: &mut BytesMut) -> Result<(), Self::Error> {
        encode_parts(&[payload.as_ref()], dst)
    }
}

/// A frame payload given as a header and a body, encoded without first joining them into one
/// allocation. Used by the game connection (`DD 01 00 <count> <op>` + packet body).
#[derive(Debug, Clone, Copy)]
pub struct Parts<'a> {
    /// The leading bytes of the payload.
    pub head: &'a [u8],
    /// The remaining bytes of the payload.
    pub body: &'a [u8],
}

impl Encoder<Parts<'_>> for FrameCodec {
    type Error = std::io::Error;

    fn encode(&mut self, payload: Parts<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        encode_parts(&[payload.head, payload.body], dst)
    }
}

fn encode_parts(parts: &[&[u8]], dst: &mut BytesMut) -> Result<(), std::io::Error> {
    let total: usize = parts.iter().map(|p| p.len()).sum();
    let len = u16::try_from(total).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "frame exceeds 65535 bytes",
        )
    })?;
    dst.reserve(2 + total);
    dst.put_u16_le(len);
    for p in parts {
        dst.extend_from_slice(p);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_whole_and_partial_frames_without_copying() {
        let mut codec = FrameCodec;
        let mut buf = BytesMut::new();
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
        buf.extend_from_slice(&[3]);
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
        buf.extend_from_slice(&[0, 1, 2]);
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
        buf.extend_from_slice(&[3, 0, 0, 0xAA]); // end of frame 1, an empty frame, a stray byte
        let first = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(&first[..], &[1, 2, 3]);
        let second = codec.decode(&mut buf).unwrap().unwrap();
        assert!(second.is_empty());
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
        assert_eq!(&buf[..], &[0xAA]);
    }

    #[test]
    fn encodes_a_payload_and_split_parts_identically() {
        let mut codec = FrameCodec;
        let mut a = BytesMut::new();
        let mut b = BytesMut::new();
        codec.encode(vec![9u8, 8, 7, 6], &mut a).unwrap();
        codec
            .encode(
                Parts {
                    head: &[9, 8],
                    body: &[7, 6],
                },
                &mut b,
            )
            .unwrap();
        assert_eq!(&a[..], &[4, 0, 9, 8, 7, 6]);
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_oversized_frames() {
        let mut codec = FrameCodec;
        let mut dst = BytesMut::new();
        let big = vec![0u8; 65_536];
        assert!(codec.encode(big, &mut dst).is_err());
        assert!(dst.is_empty());
        let max = vec![0u8; 65_535];
        assert!(codec.encode(max, &mut dst).is_ok());
        assert_eq!(dst.len(), 2 + 65_535);
    }
}
