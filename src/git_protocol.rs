//! Packet-line helpers and service type parsing for the Git smart protocol, covering both
//! `upload-pack` (fetch/clone) and `receive-pack` (push) flows.
//!
//! The Git smart protocol frames every payload as a sequence of `pkt-line` records: a
//! 4-byte ASCII hex length header followed by `length - 4` bytes of payload. A length of
//! `0000` is a flush marker. This module exposes the minimum primitives required to read
//! and write these frames and to identify which side of the protocol a request targets.

use core::fmt;
use std::str::FromStr;

use bytes::{Buf, BufMut};
use git_internal::errors::GitError;

/// Identifies the direction of a smart-protocol exchange.
///
/// Used by HTTP routers and SSH dispatchers to pick the correct backend handler.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ServiceType {
    /// Server-to-client transfer: clone, fetch, ls-remote.
    UploadPack,
    /// Client-to-server transfer: push.
    ReceivePack,
}

impl fmt::Display for ServiceType {
    /// Render the variant as the on-the-wire service name expected by Git clients
    /// (e.g. the `service=` query parameter in `info/refs`).
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ServiceType::UploadPack => write!(f, "git-upload-pack"),
            ServiceType::ReceivePack => write!(f, "git-receive-pack"),
        }
    }
}

impl FromStr for ServiceType {
    type Err = GitError;

    /// Parse a wire-format service name back into a `ServiceType`.
    ///
    /// Boundary conditions:
    /// - Comparison is case-sensitive — `"git-upload-pack"` and `"git-receive-pack"` are
    ///   the only accepted strings.
    /// - Any other input returns `GitError::InvalidArgument` with the offending value
    ///   embedded for debugging.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "git-upload-pack" => Ok(ServiceType::UploadPack),
            "git-receive-pack" => Ok(ServiceType::ReceivePack),
            _ => Err(GitError::InvalidArgument(format!(
                "Invalid service name: {}",
                s
            ))),
        }
    }
}

/// Flush packet (`0000`). Marks the end of a logical group of pkt-lines.
pub const PKT_LINE_END_MARKER: &[u8; 4] = b"0000";

use bytes::Bytes;

/// Consume a single pkt-line frame from the front of `bytes` and return its
/// `(declared_length, payload)`.
///
/// Functional scope:
/// - Reads the 4-byte ASCII hex header, decodes it as the total frame length, then
///   splits off `length - 4` bytes of payload.
/// - Mutates the input buffer in place: after a successful call, `bytes` advances past
///   the consumed frame.
///
/// Boundary conditions:
/// - Returns `(0, Bytes::new())` when `bytes` is empty so callers can use a
///   zero-length response as a stop condition.
/// - Returns `(0, Bytes::new())` when the decoded length is zero (the flush marker
///   `0000`); the leading 4 header bytes are still consumed.
/// - **Panics** when the 4-byte header is not valid UTF-8 hex. Callers must therefore
///   validate or trust the source — typically only network code that rejects malformed
///   frames upstream invokes this helper.
pub fn read_pkt_line(bytes: &mut Bytes) -> (usize, Bytes) {
    if bytes.is_empty() {
        return (0, Bytes::new());
    }
    let pkt_length_bytes = bytes.copy_to_bytes(4);
    // INVARIANT: the function's doc comment explicitly documents that
    // callers must validate the 4-byte header as UTF-8 hex. Network code
    // upstream rejects malformed frames before they reach this helper.
    let header_str = core::str::from_utf8(&pkt_length_bytes)
        .expect("pkt-line header must be 4 bytes of ASCII hex (caller contract)");
    let pkt_length = usize::from_str_radix(header_str, 16).unwrap_or_else(|_| {
        panic!("pkt-line header {pkt_length_bytes:?} is not valid hex (caller contract)")
    });
    if pkt_length == 0 {
        return (0, Bytes::new());
    }
    // Advance the buffer past the payload — the caller receives the payload slice and
    // any subsequent read continues from the next frame.
    let pkt_line = bytes.copy_to_bytes(pkt_length - 4);
    tracing::debug!("pkt line: {:?}", pkt_line);

    (pkt_length, pkt_line)
}

use bytes::BytesMut;

/// Append a UTF-8 string as a pkt-line to `pkt_line_stream`.
///
/// Functional scope:
/// - Writes the 4-byte ASCII hex length header (`buf_str.len() + 4`, including the
///   header itself) followed by the raw bytes of `buf_str`.
/// - Does **not** add a trailing newline; callers that need newline-terminated lines
///   (the common case for capability advertisements) must include the `\n` in
///   `buf_str`.
///
/// Boundary conditions:
/// - Maximum frame size is `0xffff` bytes (65,535) per the Git protocol spec; this
///   helper does not enforce that limit and will silently produce malformed frames if
///   given an oversized string. Callers must chunk longer payloads themselves.
pub fn add_pkt_line_string(pkt_line_stream: &mut BytesMut, buf_str: String) {
    let buf_str_length = buf_str.len() + 4;
    pkt_line_stream.put(Bytes::from(format!("{:04x}", buf_str_length)));
    pkt_line_stream.put(buf_str.as_bytes());
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bytes::{Bytes, BytesMut};

    use super::*;

    // ── ServiceType Display ──────────────────────────────────────────

    #[test]
    fn service_type_display_upload_pack() {
        assert_eq!(ServiceType::UploadPack.to_string(), "git-upload-pack");
    }

    #[test]
    fn service_type_display_receive_pack() {
        assert_eq!(ServiceType::ReceivePack.to_string(), "git-receive-pack");
    }

    // ── ServiceType FromStr ──────────────────────────────────────────

    #[test]
    fn service_type_from_str_upload_pack() {
        assert_eq!(
            ServiceType::from_str("git-upload-pack").unwrap(),
            ServiceType::UploadPack,
        );
    }

    #[test]
    fn service_type_from_str_receive_pack() {
        assert_eq!(
            ServiceType::from_str("git-receive-pack").unwrap(),
            ServiceType::ReceivePack,
        );
    }

    #[test]
    fn service_type_from_str_rejects_unknown() {
        assert!(ServiceType::from_str("git-unknown-pack").is_err());
    }

    #[test]
    fn service_type_from_str_is_case_sensitive() {
        assert!(ServiceType::from_str("Git-Upload-Pack").is_err());
    }

    #[test]
    fn service_type_display_roundtrips_through_from_str() {
        for svc in [ServiceType::UploadPack, ServiceType::ReceivePack] {
            let wire = svc.to_string();
            assert_eq!(ServiceType::from_str(&wire).unwrap(), svc);
        }
    }

    // ── PKT_LINE_END_MARKER ──────────────────────────────────────────

    #[test]
    fn pkt_line_end_marker_is_flush() {
        assert_eq!(PKT_LINE_END_MARKER, b"0000");
    }

    // ── read_pkt_line ────────────────────────────────────────────────

    #[test]
    fn read_pkt_line_empty_input_returns_zero() {
        let mut buf = Bytes::new();
        let (len, payload) = read_pkt_line(&mut buf);
        assert_eq!(len, 0);
        assert!(payload.is_empty());
    }

    #[test]
    fn read_pkt_line_flush_marker_returns_zero() {
        let mut buf = Bytes::from_static(b"0000");
        let (len, payload) = read_pkt_line(&mut buf);
        assert_eq!(len, 0);
        assert!(payload.is_empty());
        assert!(buf.is_empty(), "flush marker bytes should be consumed");
    }

    #[test]
    fn read_pkt_line_reads_simple_payload() {
        // "0008" means total length 8 => payload is 4 bytes: "data"
        let mut buf = Bytes::from_static(b"0008data");
        let (len, payload) = read_pkt_line(&mut buf);
        assert_eq!(len, 8);
        assert_eq!(payload.as_ref(), b"data");
        assert!(buf.is_empty());
    }

    #[test]
    fn read_pkt_line_leaves_remainder_intact() {
        // Two frames back-to-back: "0007abc" (len=7, payload="abc") + "0005x" (len=5, payload="x")
        let mut buf = Bytes::from_static(b"0007abc0005x");
        let (len1, p1) = read_pkt_line(&mut buf);
        assert_eq!(len1, 7);
        assert_eq!(p1.as_ref(), b"abc");
        let (len2, p2) = read_pkt_line(&mut buf);
        assert_eq!(len2, 5);
        assert_eq!(p2.as_ref(), b"x");
        assert!(buf.is_empty());
    }

    // ── add_pkt_line_string ──────────────────────────────────────────

    #[test]
    fn add_pkt_line_string_produces_valid_frame() {
        let mut stream = BytesMut::new();
        add_pkt_line_string(&mut stream, "hello".to_string());
        // "hello" is 5 bytes, total length = 5 + 4 = 9 => header "0009"
        assert_eq!(&stream[..4], b"0009");
        assert_eq!(&stream[4..], b"hello");
    }

    #[test]
    fn add_pkt_line_string_empty_payload() {
        let mut stream = BytesMut::new();
        add_pkt_line_string(&mut stream, String::new());
        // empty payload, total length = 0 + 4 = 4 => header "0004"
        assert_eq!(&stream[..], b"0004");
    }

    #[test]
    fn write_then_read_roundtrip() {
        let mut stream = BytesMut::new();
        add_pkt_line_string(&mut stream, "capability\n".to_string());
        let mut bytes = stream.freeze();
        let (len, payload) = read_pkt_line(&mut bytes);
        assert_eq!(len, 15); // 11 + 4
        assert_eq!(payload.as_ref(), b"capability\n");
    }
}
