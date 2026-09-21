use groundhog_json_rpc_types::{hex::Hex0x, transaction::TxHash};
use sha2::Digest;

use groundhog_tx_validation::{NetworkConfigParams, check_transaction};

use crate::{ValidatedTransaction, error::RpcServerError};

/// The `signatures` field of `SignedTransaction`, decoded for validation only.
type SignaturesField = xdrs::VecM<groundhog_xdr::transaction::DecoratedSignature>;

/// Compile-time guard tying [`SignaturesField`] to the field it stands in for.
///
/// [`validate_raw_transaction_bytes`] hand-decodes the two fields of
/// `SignedTransaction` rather than decoding the struct, so a change to the
/// signatures type — including a tighter `VecM` bound — must fail the build
/// instead of silently letting the gateway accept what a whole-struct decode
/// would reject.
const _: fn(groundhog_xdr::transaction::SignedTransaction) = |signed| {
    let _: SignaturesField = signed.signatures;
};

/// Decode, validate, and hash a raw signed transaction parameter.
///
/// `params` is the chain config reading for [`check_transaction`]; pass
/// `NetworkConfigParams::default()` when no reading is available, which
/// skips the checks that need a stated bound and applies the protocol's
/// own value for the rest.
pub fn validate_raw_transaction(
    tx_hex: &str,
    params: &NetworkConfigParams,
) -> Result<ValidatedTransaction, RpcServerError> {
    let tx_bytes = groundhog_json_rpc_types::hex::decode_prefixed_hex(tx_hex)
        .map_err(|e| RpcServerError::TxInvalid(format!("invalid hex: {e}")))?;

    validate_raw_transaction_bytes(tx_bytes, params)
}

/// Validate and hash raw signed transaction XDR bytes.
///
/// The returned hash is SHA-256 over the inner `Transaction` only — the
/// signatures are excluded, so re-signing a transaction does not change its
/// hash. Signatures are decoded to reject malformed input, but are *not*
/// cryptographically verified here; authorization is the node's job.
///
/// Accepts exactly what a whole-buffer `xdrs::txs::decode::<SignedTransaction>`
/// accepts: trailing bytes after the signatures are rejected.
///
/// A signed transaction that decodes completely but breaks an executor
/// pre-execution bound (see [`check_transaction`]) is rejected with
/// [`RpcServerError::TxInvalidDetailed`], which carries the violated bound
/// as structured error data. Framing errors come first, so that detail
/// always describes an otherwise well-formed request.
pub fn validate_raw_transaction_bytes(
    tx_bytes: Vec<u8>,
    params: &NetworkConfigParams,
) -> Result<ValidatedTransaction, RpcServerError> {
    if u32::try_from(tx_bytes.len()).is_err() {
        return Err(RpcServerError::TxInvalid("transaction too large".into()));
    }

    // `SignedTransaction` is `{ tx, signatures }`, so the inner `Transaction`
    // occupies a contiguous prefix of `tx_bytes`. Hash that prefix directly
    // instead of re-encoding the decoded value: it avoids an allocation on the
    // submit path, and it hashes exactly the bytes the client signed rather
    // than trusting XDR to round-trip bit-for-bit.
    //
    // This assumes TXS decoding stays canonical — non-zero padding is rejected,
    // so an accepted encoding is the only encoding of its value. Enabling the
    // `xdrs/permissive` feature would break that and make this hash disagree
    // with a re-encode; `prefix_hash_matches_reencoded_inner_transaction` below
    // is what catches it.
    let mut reader = xdrs::txs::buf_reader::BufReader::new(&tx_bytes);

    let tx: groundhog_xdr::transaction::Transaction = xdrs::read_xdr(&mut reader)
        .map_err(|e| RpcServerError::TxInvalid(format!("invalid XDR: {e}")))?;
    let tx_end = reader.position();

    // Decode the signatures too, and reject trailing bytes, so this accepts
    // exactly what a whole-buffer `xdrs::txs::decode::<SignedTransaction>`
    // would have accepted. The type must stay in step with the field it
    // stands in for; `signatures_field_type_is_pinned` enforces that.
    let _signatures: SignaturesField = xdrs::read_xdr(&mut reader)
        .map_err(|e| RpcServerError::TxInvalid(format!("invalid XDR: {e}")))?;
    if reader.remaining() > 0 {
        return Err(RpcServerError::TxInvalid(
            "invalid XDR: trailing bytes after signed transaction".into(),
        ));
    }

    check_transaction(&tx, params).map_err(RpcServerError::TxInvalidDetailed)?;

    let hash_bytes: [u8; 32] = sha2::Sha256::digest(&tx_bytes[..tx_end]).into();
    let hash = TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from(hash_bytes)));

    Ok(ValidatedTransaction {
        bytes: tx_bytes,
        hash,
        block_expiry_height: tx.uniquifier.block_expiry_height,
    })
}

/// Signed-transaction fixtures shared by this crate's unit tests.
#[cfg(test)]
pub(crate) mod test_fixtures {
    use groundhog_xdr::{contract::Contract, transaction::SignedTransaction};

    /// Canonical XDR/hash vector. Structurally valid with a stable hash, but
    /// not auth-valid against empty hogd-dev genesis.
    pub(crate) const CANONICAL_HASH_VECTOR_SIGNED_TX_XDR: &str = "\
bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7\
000000000000000000000000000f42400000000000000000000f424000000000000000010000000000000001\
0000000050ec72dbafd4898c5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a000000000000\
000000000001000000000000000041d0144af2776cae9804ed899018a6bc17a5705d74c84e02556b9695\
031c9d3fe2554e5ce62ceadc95ba43c4aa0884cd2f9ebea4e3845ef877761a6222e91f0d";

    /// Hash of the inner transaction of the canonical vector.
    pub(crate) const CANONICAL_HASH: &str =
        "9dc7ad0fc4f74a43a62e047c0d6885c8e4df2d4840298ecd52cf157681157ed3";

    /// The canonical vector, decoded.
    pub(crate) fn canonical_signed_transaction() -> SignedTransaction {
        let canonical = hex::decode(CANONICAL_HASH_VECTOR_SIGNED_TX_XDR)
            .expect("bug: canonical hash vector should be valid hex");
        xdrs::txs::decode(&canonical).expect("bug: canonical hash vector should decode")
    }

    /// The canonical vector with `contracts` attached in `contracts_to_deploy`.
    ///
    /// Built through the XDR types rather than by splicing hex so the fixture
    /// tracks the schema instead of hard-coded field offsets.
    pub(crate) fn signed_transaction_with_contracts(contracts: Vec<Contract>) -> SignedTransaction {
        let mut signed = canonical_signed_transaction();
        signed.tx.contracts_to_deploy = xdrs::VecM(contracts);
        signed
    }

    /// `signed_transaction_with_contracts`, encoded as raw signed-transaction bytes.
    pub(crate) fn signed_tx_bytes_with_contracts(contracts: Vec<Contract>) -> Vec<u8> {
        xdrs::txs::encode(&signed_transaction_with_contracts(contracts))
            .expect("bug: re-encoded signed tx should encode")
    }
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::error::TxInvalidDetail;
    use groundhog_xdr::contract::{Contract, ContractBlob};
    use xdr_common::BytesM;

    use super::test_fixtures::{
        CANONICAL_HASH, CANONICAL_HASH_VECTOR_SIGNED_TX_XDR, canonical_signed_transaction,
        signed_tx_bytes_with_contracts,
    };
    use super::*;
    use groundhog_xdr::contract::kDefaultMaxContractSizeBytes;

    fn rv32(size: usize, fill: u8) -> Contract {
        Contract::kRv32 {
            rv32_contract: ContractBlob {
                data: BytesM(vec![fill; size]),
            },
        }
    }

    /// Re-encodes the canonical vector carrying `count` copies of its signature.
    ///
    /// Built through the XDR types rather than by splicing hex so the variants
    /// track the schema instead of hard-coded field offsets.
    fn signed_tx_with_signature_count(count: usize) -> Vec<u8> {
        let mut signed = canonical_signed_transaction();

        let signature = signed.signatures.0[0].clone();
        signed.signatures = xdrs::VecM(vec![signature; count]);

        xdrs::txs::encode(&signed).expect("bug: re-encoded signed tx should encode")
    }

    #[test]
    fn prefix_hash_matches_reencoded_inner_transaction() {
        // Differential check against the algorithm this replaced: decode the
        // whole struct and hash a re-encode of the inner tx. Pins the relation
        // rather than the constant, so a `SignedTransaction` layout change
        // fails with a diagnosis instead of an unexplained hash diff.
        for count in [0usize, 1, 2, 5] {
            let bytes = signed_tx_with_signature_count(count);
            let signed: groundhog_xdr::transaction::SignedTransaction =
                xdrs::txs::decode(&bytes).expect("bug: re-encoded signed tx should decode");
            let reencoded = xdrs::txs::encode(&signed.tx).expect("bug: inner tx should encode");
            let expected: [u8; 32] = sha2::Sha256::digest(&reencoded).into();

            let validated = validate_raw_transaction_bytes(bytes, &NetworkConfigParams::default())
                .expect("bug: re-encoded signed tx should validate");

            assert_eq!(
                validated.hash.0.0.as_ref(),
                expected.as_slice(),
                "hashed prefix diverged from the canonical Transaction encoding \
                 at {count} signature(s); SignedTransaction layout may have changed"
            );
        }
    }

    #[test]
    fn signature_count_does_not_affect_the_hash() {
        // The hash covers the inner transaction only, so adding, removing, or
        // replacing signatures must leave it untouched.
        for count in [0usize, 1, 2, 5] {
            let validated = validate_raw_transaction_bytes(
                signed_tx_with_signature_count(count),
                &NetworkConfigParams::default(),
            )
            .expect("bug: re-encoded signed tx should validate");

            assert_eq!(
                hex::encode(validated.hash.0.0.as_ref()),
                CANONICAL_HASH,
                "hash changed at {count} signature(s)"
            );
        }
    }

    #[test]
    fn invalid_hex_is_rejected() {
        let err = validate_raw_transaction("not-valid-hex", &NetworkConfigParams::default())
            .expect_err("bug: invalid hex should be rejected");

        assert!(matches!(err, RpcServerError::TxInvalid(_)));
    }

    #[test]
    fn invalid_xdr_is_rejected() {
        let err = validate_raw_transaction("0x00", &NetworkConfigParams::default())
            .expect_err("bug: invalid XDR should fail");

        assert!(matches!(err, RpcServerError::TxInvalid(_)));
    }

    #[test]
    fn exposes_block_expiry_height_from_decoded_transaction() {
        let tx = validate_raw_transaction(
            CANONICAL_HASH_VECTOR_SIGNED_TX_XDR,
            &NetworkConfigParams::default(),
        )
        .expect("bug: canonical hash vector should validate as XDR");

        assert_eq!(*tx.block_expiry_height, 1_000_000);
    }

    #[test]
    fn computes_canonical_hash_from_inner_transaction() {
        let tx = validate_raw_transaction(
            CANONICAL_HASH_VECTOR_SIGNED_TX_XDR,
            &NetworkConfigParams::default(),
        )
        .expect("bug: canonical hash vector should validate as XDR");

        assert_eq!(hex::encode(tx.hash.0.0.as_ref()), CANONICAL_HASH);
    }

    #[test]
    fn uppercase_0x_prefix_is_accepted() {
        let tx = validate_raw_transaction(
            &format!("0X{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}"),
            &NetworkConfigParams::default(),
        )
        .expect("bug: uppercase 0X prefix should be accepted");

        assert_eq!(hex::encode(tx.hash.0.0.as_ref()), CANONICAL_HASH);
    }

    #[test]
    fn trailing_bytes_after_signed_transaction_are_rejected() {
        let err = validate_raw_transaction(
            &format!("{CANONICAL_HASH_VECTOR_SIGNED_TX_XDR}00"),
            &NetworkConfigParams::default(),
        )
        .expect_err("bug: trailing bytes should be rejected");

        assert!(matches!(err, RpcServerError::TxInvalid(_)));
    }

    #[test]
    fn truncated_signatures_are_rejected() {
        // Drop 8 hex chars, i.e. 4 bytes off the trailing 64-byte signature, so
        // the transaction prefix still decodes but the signature runs short.
        let truncated =
            &CANONICAL_HASH_VECTOR_SIGNED_TX_XDR[..CANONICAL_HASH_VECTOR_SIGNED_TX_XDR.len() - 8];
        let err = validate_raw_transaction(truncated, &NetworkConfigParams::default())
            .expect_err("bug: truncated signatures should be rejected");

        assert!(matches!(err, RpcServerError::TxInvalid(_)));
    }

    #[test]
    fn bound_violation_is_rejected_with_structured_detail() {
        let params = NetworkConfigParams {
            max_new_contracts_per_tx: Some(1),
            ..NetworkConfigParams::default()
        };
        let bytes = signed_tx_bytes_with_contracts(vec![rv32(8, 1), rv32(8, 2)]);

        let err = validate_raw_transaction_bytes(bytes, &params)
            .expect_err("bug: two contracts should exceed a cap of one");

        assert_eq!(
            err,
            RpcServerError::TxInvalidDetailed(TxInvalidDetail::TooManyContracts {
                attached: 2,
                max: 1
            })
        );
    }

    #[test]
    fn framing_errors_are_reported_before_bound_violations() {
        // Structured bound detail is only useful for a request that is
        // otherwise well-formed; trailing garbage after the signatures must
        // surface as the framing error it is.
        let params = NetworkConfigParams {
            max_new_contracts_per_tx: Some(1),
            ..NetworkConfigParams::default()
        };
        let mut bytes = signed_tx_bytes_with_contracts(vec![rv32(8, 1), rv32(8, 2)]);
        bytes.push(0);

        let err = validate_raw_transaction_bytes(bytes, &params)
            .expect_err("bug: trailing bytes should be rejected");

        assert_eq!(
            err,
            RpcServerError::TxInvalid(
                "invalid XDR: trailing bytes after signed transaction".into()
            )
        );
    }

    #[test]
    fn oversized_contract_is_rejected_without_a_config_reading() {
        // No reading leaves the protocol's default cap in force.
        let bytes = signed_tx_bytes_with_contracts(vec![rv32(
            kDefaultMaxContractSizeBytes as usize + 1,
            1,
        )]);

        let err = validate_raw_transaction_bytes(bytes, &NetworkConfigParams::default())
            .expect_err("bug: oversized contract should be rejected");

        assert!(matches!(
            err,
            RpcServerError::TxInvalidDetailed(TxInvalidDetail::ContractTooLarge { index: 0, .. })
        ));
    }

    #[test]
    fn attached_contracts_within_bounds_still_hash_the_inner_transaction() {
        let params = NetworkConfigParams {
            max_new_contracts_per_tx: Some(2),
            ..NetworkConfigParams::default()
        };
        let signed = super::test_fixtures::signed_transaction_with_contracts(vec![rv32(8, 1)]);
        let bytes = xdrs::txs::encode(&signed).unwrap();
        let reencoded = xdrs::txs::encode(&signed.tx).unwrap();
        let expected: [u8; 32] = sha2::Sha256::digest(&reencoded).into();

        let validated = validate_raw_transaction_bytes(bytes, &params)
            .expect("bug: in-bounds attachments should validate");

        assert_eq!(validated.hash.0.0.as_ref(), expected.as_slice());
    }

    #[test]
    fn empty_hex_input_is_rejected() {
        let err = validate_raw_transaction("", &NetworkConfigParams::default())
            .expect_err("bug: empty hex input should fail XDR decode");

        assert!(matches!(err, RpcServerError::TxInvalid(_)));
    }
}
