//! Local ML-DSA-65 signer backed by the `ml-dsa` crate.

use ml_dsa::signature::{Keypair as _, Signer as _};
use ml_dsa::{B32, MlDsa65};
use thruput_xdr::types::{MlDsa65PublicKey, MlDsa65Signature, PublicKey, Signature};
use zeroize::Zeroizing;

use crate::SigningError;

/// Length of an ML-DSA-65 public key in bytes.
pub const MLDSA65_PUBLIC_KEY_LEN: usize = 1952;
/// Length of an ML-DSA-65 signature in bytes.
pub const MLDSA65_SIGNATURE_LEN: usize = 3309;
/// Length of the FIPS 204 key-generation seed `ξ` in bytes.
pub const MLDSA65_SEED_LEN: usize = 32;

/// Local signer holding an ML-DSA-65 key expanded from its 32-byte FIPS 204
/// seed.
///
/// Signs with pure ML-DSA (not HashML-DSA), an empty context string, and the
/// deterministic variant, so a seed and message always give the same
/// signature. The expanded key zeroizes on drop; store only the seed.
pub struct MlDsa65Signer {
    // Boxed: the expanded key is tens of kilobytes.
    signing_key: Box<ml_dsa::SigningKey<MlDsa65>>,
    public_key: Box<[u8; MLDSA65_PUBLIC_KEY_LEN]>,
}

impl std::fmt::Debug for MlDsa65Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlDsa65Signer").finish_non_exhaustive()
    }
}

impl MlDsa65Signer {
    /// Expands a signer from the FIPS 204 `ML-DSA.KeyGen_internal` seed `ξ`.
    pub fn from_seed(seed: &[u8; MLDSA65_SEED_LEN]) -> Self {
        let seed = Zeroizing::new(B32::from(*seed));
        let signing_key = Box::new(ml_dsa::SigningKey::<MlDsa65>::from_seed(&seed));
        let encoded = signing_key.verifying_key().encode();
        let mut public_key = Box::new([0; MLDSA65_PUBLIC_KEY_LEN]);
        public_key.copy_from_slice(encoded.as_slice());
        Self {
            signing_key,
            public_key,
        }
    }

    /// Returns the seed this signer was expanded from. Treat it as secret.
    pub fn to_seed(&self) -> Zeroizing<[u8; MLDSA65_SEED_LEN]> {
        let seed = Zeroizing::new(self.signing_key.to_seed());
        let mut bytes = Zeroizing::new([0; MLDSA65_SEED_LEN]);
        bytes.copy_from_slice(seed.as_slice());
        bytes
    }

    /// Returns the encoded 1952-byte public key.
    pub fn public_key_bytes(&self) -> &[u8; MLDSA65_PUBLIC_KEY_LEN] {
        &self.public_key
    }
}

impl crate::Signer for MlDsa65Signer {
    fn public_key(&self) -> PublicKey {
        PublicKey::MlDsa65 {
            mldsa65: MlDsa65PublicKey::new(*self.public_key),
        }
    }

    fn sign(&self, message: &[u8]) -> Result<Signature, SigningError> {
        let signature = self
            .signing_key
            .try_sign(message)
            .map_err(|_| SigningError::SignatureFailed)?;
        let encoded = signature.encode();
        let mut bytes = [0; MLDSA65_SIGNATURE_LEN];
        bytes.copy_from_slice(encoded.as_slice());
        Ok(Signature::MlDsa65 {
            mldsa65: MlDsa65Signature::new(bytes),
        })
    }
}

/// Verifies a pure ML-DSA-65 signature with an empty context string.
///
/// Returns `false` for malformed keys or signatures as well as for
/// signatures that do not verify.
pub fn verify_mldsa65(
    public_key: &[u8; MLDSA65_PUBLIC_KEY_LEN],
    message: &[u8],
    signature: &[u8; MLDSA65_SIGNATURE_LEN],
) -> bool {
    let Ok(encoded_key) = ml_dsa::EncodedVerifyingKey::<MlDsa65>::try_from(&public_key[..]) else {
        return false;
    };
    let verifying_key = ml_dsa::VerifyingKey::<MlDsa65>::decode(&encoded_key);
    let Ok(encoded_signature) = ml_dsa::EncodedSignature::<MlDsa65>::try_from(&signature[..])
    else {
        return false;
    };
    let Some(signature) = ml_dsa::Signature::<MlDsa65>::decode(&encoded_signature) else {
        return false;
    };
    verifying_key.verify_with_context(message, &[], &signature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BuildCallTransaction, PubkeyAccount, SignTransactionParams, Signer, build_call_transaction,
        sign_transaction,
    };
    use thruput_xdr::types::{NetworkHash, SHA256Hash};

    fn transaction_for(account: &PubkeyAccount) -> thruput_xdr::transaction::Transaction {
        build_call_transaction(BuildCallTransaction {
            invoked_address: account.address(),
            entrypoint: [0; 8].into(),
            calldata: vec![],
            gas_limit: 1,
            block_expiry_height: 1,
            nonce: 1,
            auth_claim: account.auth_claim(),
        })
    }

    #[test]
    fn seed_round_trips_and_signatures_verify() {
        let signer = MlDsa65Signer::from_seed(&[7; MLDSA65_SEED_LEN]);
        assert_eq!(*signer.to_seed(), [7; MLDSA65_SEED_LEN]);
        let Signature::MlDsa65 { mldsa65 } = signer.sign(b"message").unwrap() else {
            panic!("expected an ML-DSA-65 signature");
        };
        assert!(verify_mldsa65(
            signer.public_key_bytes(),
            b"message",
            &mldsa65
        ));
        assert!(!verify_mldsa65(
            signer.public_key_bytes(),
            b"other",
            &mldsa65
        ));
    }

    #[test]
    fn signing_attaches_the_claimed_key_and_rejects_another() {
        let signer = MlDsa65Signer::from_seed(&[7; MLDSA65_SEED_LEN]);
        let account = PubkeyAccount::new(signer.public_key(), 3);
        let network = NetworkHash::new(SHA256Hash::new([1; 32]));

        let envelope = sign_transaction(
            transaction_for(&account),
            &signer,
            SignTransactionParams::default(),
            &network,
        )
        .unwrap();
        assert_eq!(envelope.pubkey_claim_key, Some(signer.public_key()));

        let other = MlDsa65Signer::from_seed(&[8; MLDSA65_SEED_LEN]);
        assert!(matches!(
            sign_transaction(
                transaction_for(&account),
                &other,
                SignTransactionParams::default(),
                &network,
            ),
            Err(SigningError::SignerMismatch)
        ));
    }
}
