//! Conversions between the protocol token's wire form and its text form.
//!
//! `ProtocolCommit` is a fixed 20-byte array on the wire, but the same bytes
//! are printable ASCII and reach this workspace as text: the runtime
//! manifest's `protocol` field is JSON, and `--hogd-protocol` is a
//! command-line string. Peers compare the token byte for byte, so text that
//! is not exactly the wire width cannot name any token a peer could accept.

use groundhog_xdr::network::ProtocolCommit;
use thiserror::Error;

/// Number of bytes the token occupies on the wire.
pub const PROTOCOL_COMMIT_LEN: usize = std::mem::size_of::<ProtocolCommit>();

/// Text offered as a protocol token was not the wire width.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "protocol token must be exactly {PROTOCOL_COMMIT_LEN} bytes, but {value:?} is {actual}; \
     it identifies a protocol revision, so a value of any other width cannot match a peer"
)]
pub struct ProtocolTokenWidthError {
    /// The text that was offered.
    pub value: String,
    /// Its width in bytes.
    pub actual: usize,
}

/// The token as printable text.
pub fn protocol_text(protocol: &ProtocolCommit) -> String {
    String::from_utf8_lossy(protocol).into_owned()
}

/// The wire form of a token given as text.
///
/// Rejects anything that is not exactly [`PROTOCOL_COMMIT_LEN`] bytes rather
/// than padding or truncating to fit. Silently reshaping the text would turn a
/// malformed override into a well-formed token that simply fails the intro
/// handshake, reported as a protocol mismatch against a value the operator
/// never wrote.
pub fn protocol_from_text(text: &str) -> Result<ProtocolCommit, ProtocolTokenWidthError> {
    ProtocolCommit::try_from(text.as_bytes()).map_err(|_| ProtocolTokenWidthError {
        value: text.to_owned(),
        actual: text.len(),
    })
}
