//! Conversions between the protocol token's wire form and its text form.
//!
//! `ProtocolCommit` is a fixed 20-byte array on the wire, but the same bytes
//! are printable ASCII and reach this workspace as text: the runtime
//! manifest's `protocol` field is JSON, and `--hogd-protocol-override` is a
//! command-line string. Peers compare the token byte for byte, so both sides
//! of that boundary have to agree on exactly how text becomes bytes.

use groundhog_xdr::network::ProtocolCommit;

/// Number of bytes the token occupies on the wire.
pub const PROTOCOL_COMMIT_LEN: usize = std::mem::size_of::<ProtocolCommit>();

/// The byte the wire form pads a short token with, matching hogd's
/// `protocol_from_text` in `hogd/xdr/protocol_token.h`. Two implementations
/// that pad differently would derive unequal tokens from the same text and
/// reject each other at the intro RPC.
const PAD: u8 = b'_';

/// The token as printable text, with padding retained.
///
/// hogd's own token fills the array exactly, so in production this is the
/// whole 20 characters. A shorter token -- a test fixture, or a hand-written
/// manifest -- keeps its trailing padding here rather than being trimmed,
/// because the padding is part of what peers compare.
pub fn protocol_text(protocol: &ProtocolCommit) -> String {
    String::from_utf8_lossy(protocol).into_owned()
}

/// The wire form of a token given as text, padded to width.
///
/// Text longer than the array is truncated rather than rejected: the result
/// simply will not match a peer, which is the same outcome as any other
/// mismatched token and is reported by the intro RPC that compares them.
pub fn protocol_from_text(text: &str) -> ProtocolCommit {
    let mut protocol = [PAD; PROTOCOL_COMMIT_LEN];
    let bytes = text.as_bytes();
    let width = bytes.len().min(PROTOCOL_COMMIT_LEN);
    protocol[..width].copy_from_slice(&bytes[..width]);
    protocol
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundhog_xdr::PROTOCOL_VERSION;

    #[test]
    fn this_builds_token_fills_the_array_exactly() {
        // The real token is sized so that no padding is involved; if that ever
        // stops being true the round-trip below is testing the wrong thing.
        assert_eq!(PROTOCOL_VERSION.len(), PROTOCOL_COMMIT_LEN);
        assert!(!PROTOCOL_VERSION.contains(&PAD));
        assert_eq!(
            protocol_from_text(&protocol_text(&PROTOCOL_VERSION)),
            PROTOCOL_VERSION
        );
    }

    #[test]
    fn short_text_pads_to_width() {
        let protocol = protocol_from_text("hogp/test");
        assert_eq!(protocol.len(), PROTOCOL_COMMIT_LEN);
        assert_eq!(&protocol[..9], b"hogp/test");
        assert!(protocol[9..].iter().all(|&b| b == PAD));
    }

    #[test]
    fn padding_survives_the_round_trip() {
        // Peers compare the padded bytes, so text() must not trim: a trimmed
        // value fed back through from_text() would still pad to the same
        // array, but callers that compare the text form would disagree.
        assert_eq!(
            protocol_text(&protocol_from_text("hogp/test")),
            "hogp/test___________"
        );
    }

    #[test]
    fn overlong_text_truncates_to_width() {
        let protocol = protocol_from_text("hogp/0123456789abcdefghij");
        assert_eq!(protocol.len(), PROTOCOL_COMMIT_LEN);
        assert_eq!(&protocol, b"hogp/0123456789abcde");
    }

    #[test]
    fn empty_text_is_all_padding() {
        assert_eq!(protocol_from_text(""), [PAD; PROTOCOL_COMMIT_LEN]);
    }
}
