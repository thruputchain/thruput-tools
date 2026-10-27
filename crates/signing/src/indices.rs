//! Strongly-typed indices used when constructing signatures.

/// Identifier for the public key used to produce a signature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PublicKeyId(pub u32);
