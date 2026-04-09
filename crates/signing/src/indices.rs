//! Strongly-typed indices used when constructing signatures.

/// Index into the transaction auth-claims vector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthClaimIndex(pub u32);

/// Identifier for the public key used to produce a signature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PublicKeyId(pub u32);
