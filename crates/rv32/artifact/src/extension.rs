//! The v6 artifact extension list: data carried after initial memory that the
//! runtime never loads into guest memory.
//!
//! The list is XDR, specified in the protocol's `contract.x`:
//!
//! ```text
//! /// What an RV32 artifact extension carries. Values are permanent: a value
//! /// is never reused.
//! enum Rv32ArtifactExtensionKind {
//!   /// body = XDR(Rv32ContractSpecExtension).
//!   kRv32ExtensionContractSpec = 1
//! };
//!
//! /// One extension. `kind` is an `Rv32ArtifactExtensionKind`, typed as
//! /// `unsigned int` so a decoder accepts kinds it does not know.
//! struct Rv32ArtifactExtension {
//!   unsigned int kind;
//!   opaque body<>;
//! };
//!
//! /// Strictly ascending by `kind`: at most one extension of each kind, and
//! /// exactly one encoding of any set of extensions.
//! typedef Rv32ArtifactExtension Rv32ArtifactExtensions<>;
//!
//! /// The body of a `kRv32ExtensionContractSpec` extension.
//! struct Rv32ContractSpecExtension {
//!   unsigned int schema_version;  // 1: spec = XDR(ContractSpec)
//!   opaque spec<>;
//! };
//! ```
//!
//! The chain checks only the list's framing: it decodes as exactly one
//! `Rv32ArtifactExtensions` with zero XDR padding, every `kind` is nonzero,
//! and kinds are strictly ascending. It never interprets a `body`, so adding a
//! kind needs no runtime change. Bodies are whatever the uploader supplied;
//! readers validate the kinds they understand and ignore the rest.

use crate::error::{Result, invalid, verify};

/// `kRv32ExtensionContractSpec`: the body is an XDR `Rv32ContractSpecExtension`.
pub const EXTENSION_KIND_CONTRACT_SPEC: u32 = 1;

/// One borrowed extension: its kind and its opaque body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactExtension<'a> {
    /// Nonzero kind. Kinds this crate does not know are valid.
    pub kind: u32,
    /// The extension's body, not interpreted by the container.
    pub body: &'a [u8],
}

/// A borrowed contract spec: the decoded body of a
/// [`EXTENSION_KIND_CONTRACT_SPEC`] extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactSpec<'a> {
    /// Schema of `bytes`. Unknown versions are valid; a reader reports them as
    /// unsupported rather than as an absent spec.
    pub schema_version: u32,
    /// The serialized spec.
    pub bytes: &'a [u8],
}

const XDR_UNIT: usize = 4;

fn xdr_padding(len: usize) -> usize {
    (XDR_UNIT - len % XDR_UNIT) % XDR_UNIT
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_opaque(out: &mut Vec<u8>, bytes: &[u8], what: &str) -> Result<()> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| invalid(format!("{what} does not fit in an XDR opaque")))?;
    put_u32(out, len);
    out.extend_from_slice(bytes);
    out.resize(out.len() + xdr_padding(bytes.len()), 0);
    Ok(())
}

/// A strict XDR reader over borrowed bytes.
struct Reader<'a> {
    bytes: &'a [u8],
    what: &'static str,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        if len > self.bytes.len() {
            return Err(verify(format!("{} truncated", self.what)));
        }
        let (head, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        Ok(head)
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(XDR_UNIT)?;
        Ok(u32::from_be_bytes(
            bytes.try_into().expect("bug: 4-byte slice"),
        ))
    }

    fn opaque(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        let body = self.take(len)?;
        if self.take(xdr_padding(len))?.iter().any(|&b| b != 0) {
            return Err(verify(format!("{} has nonzero XDR padding", self.what)));
        }
        Ok(body)
    }

    fn finish(self) -> Result<()> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(verify(format!("{} has trailing bytes", self.what)))
        }
    }
}

/// XDR-encode an extension list, refusing one the chain would reject.
pub(crate) fn encode_extensions(extensions: &[ArtifactExtension<'_>]) -> Result<Vec<u8>> {
    let count = u32::try_from(extensions.len())
        .map_err(|_| invalid("extension count does not fit in u32"))?;
    let mut out = Vec::new();
    put_u32(&mut out, count);
    let mut previous = 0;
    for extension in extensions {
        if extension.kind == 0 {
            return Err(invalid("extension kind 0 is reserved"));
        }
        if extension.kind <= previous {
            return Err(invalid("extension kinds must be strictly ascending"));
        }
        previous = extension.kind;
        put_u32(&mut out, extension.kind);
        put_opaque(&mut out, extension.body, "extension body")?;
    }
    Ok(out)
}

/// Decode an extension list, applying exactly the chain's framing rules.
pub(crate) fn decode_extensions(bytes: &[u8]) -> Result<Vec<ArtifactExtension<'_>>> {
    let mut reader = Reader {
        bytes,
        what: "extension list",
    };
    let count = reader.u32()? as usize;
    // Each entry takes at least 8 bytes; bound the allocation by what is left.
    if count > reader.bytes.len() / (2 * XDR_UNIT) {
        return Err(verify("extension list truncated"));
    }
    let mut extensions = Vec::with_capacity(count);
    let mut previous = 0;
    for _ in 0..count {
        let kind = reader.u32()?;
        if kind == 0 {
            return Err(verify("extension kind 0 is reserved"));
        }
        if kind <= previous {
            return Err(verify("extension kinds must be strictly ascending"));
        }
        previous = kind;
        let body = reader.opaque()?;
        extensions.push(ArtifactExtension { kind, body });
    }
    reader.finish()?;
    Ok(extensions)
}

/// XDR-encode a contract spec as the body of a
/// [`EXTENSION_KIND_CONTRACT_SPEC`] extension.
pub fn encode_spec_extension(spec: ArtifactSpec<'_>) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(2 * XDR_UNIT + spec.bytes.len() + XDR_UNIT);
    put_u32(&mut out, spec.schema_version);
    put_opaque(&mut out, spec.bytes, "contract spec")?;
    Ok(out)
}

/// Decode the body of a [`EXTENSION_KIND_CONTRACT_SPEC`] extension.
pub fn decode_spec_extension(body: &[u8]) -> Result<ArtifactSpec<'_>> {
    let mut reader = Reader {
        bytes: body,
        what: "contract spec extension",
    };
    let schema_version = reader.u32()?;
    let bytes = reader.opaque()?;
    reader.finish()?;
    Ok(ArtifactSpec {
        schema_version,
        bytes,
    })
}
