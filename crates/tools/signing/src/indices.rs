//! Strongly-typed indices used when constructing signatures.

/// Legacy index type kept for callers that still track pre-0.10 auth-claim
/// positions out of band.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthClaimIndex(pub u32);

/// Identifier for the public key used to produce a signature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PublicKeyId(pub u32);
