//! Shared helpers for the dev-wallet contract wire format.

use std::path::Path;

use groundhog_xdr::contract::{Contract, ContractBlob};
use groundhog_xdr::types::{Ed25519PublicKey, SHA256Hash};
use sha2::{Digest, Sha256};
use xdr_common::BytesM;

use crate::SigningError;

/// Encode the dev-wallet `create_wallet` calldata tuple.
pub fn create_wallet_calldata(
    deploy_nonce: &SHA256Hash,
    contract_hash: &SHA256Hash,
    owner: Ed25519PublicKey,
) -> Result<Vec<u8>, SigningError> {
    xdrs::cxs::encode(&(deploy_nonce, contract_hash, &owner))
        .map_err(|error| SigningError::XdrEncode(error.to_string()))
}

/// Build a dev-wallet contract value from an artifact path and bytes.
pub fn dev_wallet_contract(
    artifact_path: &Path,
    artifact_bytes: Vec<u8>,
) -> Result<Contract, SigningError> {
    match artifact_path
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("rv32a") => Ok(Contract::kRv32 {
            rv32_contract: ContractBlob {
                data: BytesM(artifact_bytes),
            },
        }),
        _ => Err(SigningError::UnsupportedDevWalletArtifact(
            artifact_path.to_path_buf(),
        )),
    }
}

/// Compute the dev-wallet contract hash used for deterministic wallet creation.
pub fn wallet_contract_hash(contract: &Contract) -> Result<SHA256Hash, SigningError> {
    let contract_xdr =
        xdrs::txs::encode(contract).map_err(|error| SigningError::XdrEncode(error.to_string()))?;
    let bytes: [u8; 32] = Sha256::digest(contract_xdr).into();
    Ok(SHA256Hash::from(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_wallet_calldata_round_trips_expected_tuple() {
        let nonce = SHA256Hash::from([0x11; 32]);
        let contract_hash = SHA256Hash::from([0x22; 32]);
        let owner = Ed25519PublicKey::from([0x33; 32]);

        let encoded = create_wallet_calldata(&nonce, &contract_hash, owner)
            .expect("bug: create_wallet calldata should encode");
        let decoded: (SHA256Hash, SHA256Hash, Ed25519PublicKey) =
            xdrs::cxs::decode(&encoded).expect("bug: create_wallet calldata should decode");

        assert_eq!(decoded, (nonce, contract_hash, owner));
    }

    #[test]
    fn dev_wallet_contract_uses_rv32_variant_for_rv32a_artifacts() {
        let contract = dev_wallet_contract(Path::new("dev_wallet.rv32a"), vec![1, 2, 3])
            .expect("bug: RV32 dev-wallet artifact should be accepted");

        assert!(matches!(contract, Contract::kRv32 { .. }));
    }

    fn assert_rejected_artifact(path: &Path) {
        let error = dev_wallet_contract(path, vec![1, 2, 3]).unwrap_err();

        assert!(matches!(
            error,
            SigningError::UnsupportedDevWalletArtifact(error_path) if error_path == path
        ));
    }

    #[test]
    fn dev_wallet_contract_rejects_unknown_or_missing_artifact_extension() {
        assert_rejected_artifact(Path::new("contracts/dev_wallet.bin"));
        assert_rejected_artifact(Path::new("contracts/dev_wallet"));
    }

    #[cfg(unix)]
    #[test]
    fn dev_wallet_contract_rejects_non_utf8_artifact_extension() {
        use std::os::unix::ffi::OsStringExt;

        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(
            b"contracts/dev_wallet.\xff".to_vec(),
        ));
        assert_rejected_artifact(&path);
    }
}
