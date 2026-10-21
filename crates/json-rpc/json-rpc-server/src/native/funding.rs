//! Optional funding producers. These calls never imply transaction admission.

use async_trait::async_trait;
use jsonrpsee::core::RpcResult;
use thruput_json_rpc_types::{requests as input, responses as out};

/// Configured funding operations, independent of transient backend health.
#[derive(Debug, Clone, Copy)]
pub enum FundingCapabilities {
    /// No faucet or local funding service is configured.
    None,
    /// A hosted faucet supports one logical funding attempt per request.
    Faucet,
    /// A validated local signer and journal support faucet and explicit funding.
    Local,
}

/// Backend-specific funding operations with native error evidence.
#[async_trait]
pub trait NativeFundingPlane: Send + Sync + 'static {
    /// The operations this configured producer implements, even during an outage.
    fn capabilities(&self) -> FundingCapabilities;
    /// Advance a faucet attempt once. Callers must not automatically repeat it.
    async fn request_airdrop(
        &self,
        request: input::AirdropRequest,
    ) -> RpcResult<out::AirdropAttempt>;
    /// Submit or reconcile the exact retained local funding request.
    async fn fund_account(
        &self,
        request: input::FundAccountRequest,
    ) -> RpcResult<out::FundingSubmission>;
}

#[async_trait]
impl NativeFundingPlane for () {
    fn capabilities(&self) -> FundingCapabilities {
        FundingCapabilities::None
    }
    async fn request_airdrop(&self, _: input::AirdropRequest) -> RpcResult<out::AirdropAttempt> {
        Err(super::service::method_not_found())
    }
    async fn fund_account(
        &self,
        _: input::FundAccountRequest,
    ) -> RpcResult<out::FundingSubmission> {
        Err(super::service::method_not_found())
    }
}

use crate::RpcServerError;
use jsonrpsee::types::ErrorObjectOwned;
use thruput_json_rpc_types::{
    ContractAddress,
    airdrop::{AirdropAttemptResponse, FundAccountResponse, LocalFundingErrorData},
    error::native::{
        BackendDataInvalidReason, ErrorData, FundingAttemptStoppedData, FundingCause, codes,
    },
    receipt::ExecStatus,
};

fn invalid(message: &str) -> RpcServerError {
    RpcServerError::invalid_data(BackendDataInvalidReason::InconsistentResult, message)
}

fn amount(value: &str) -> Result<u128, RpcServerError> {
    value
        .parse::<u128>()
        .ok()
        .filter(|amount| *amount > 0 && amount.to_string() == value)
        .ok_or_else(|| invalid("funding response has an invalid raw amount"))
}

/// Project existing faucet results without inferring admission or manufacturing output.
pub fn airdrop_result(
    recipient: ContractAddress,
    result: AirdropAttemptResponse,
) -> Result<out::AirdropAttempt, RpcServerError> {
    if result.recipient() != &recipient {
        return Err(invalid("funding response recipient mismatch"));
    }
    let raw_amount = amount(result.raw_amount())?;
    match result {
        AirdropAttemptResponse::Pending {
            recipient,
            tx_hash,
            retry_after_seconds,
            ..
        } => Ok(out::AirdropAttempt::Pending {
            recipient,
            raw_amount,
            transaction_hash: tx_hash,
            retry_after_seconds,
        }),
        AirdropAttemptResponse::Succeeded {
            recipient,
            tx_hash,
            height,
            status: ExecStatus::Ok,
            return_data: Some(return_data),
            ..
        } if *height >= 0 => Ok(out::AirdropAttempt::Succeeded {
            recipient,
            raw_amount,
            transaction_hash: tx_hash,
            block_height: height,
            execution: out::AirdropExecution::Return { return_data },
        }),
        _ => Err(invalid(
            "succeeded funding requires a committed return with observed output",
        )),
    }
}

fn validate_signed(evidence: &LocalFundingErrorData) -> Result<(), RpcServerError> {
    // These retained funding transactions contain no code attachments. Use the
    // canonical envelope/hash validator with the protocol's unstated-bound policy.
    let validated = crate::tx::validate_raw_transaction_bytes(
        evidence.signed_transaction.0.clone(),
        &thruput_tx_validation::NetworkConfigParams::default(),
    )
    .map_err(|_| invalid("funding evidence has invalid signed bytes"))?;
    if validated.hash != evidence.tx_hash {
        return Err(invalid("funding evidence hash mismatch"));
    }
    Ok(())
}

/// Preserve the exact signed local submission, validating its identity before returning it.
pub fn submission_result(
    request: &input::FundAccountRequest,
    result: FundAccountResponse,
) -> Result<out::FundingSubmission, RpcServerError> {
    if result.recipient != request.recipient || amount(&result.raw_amount)? != request.raw_amount {
        return Err(invalid("funding response does not match its request"));
    }
    validate_signed(&LocalFundingErrorData::new(
        result.tx_hash,
        result.signed_transaction.clone(),
    ))?;
    Ok(out::FundingSubmission {
        recipient: result.recipient,
        raw_amount: request.raw_amount,
        transaction_hash: result.tx_hash,
        signed_transaction: result.signed_transaction,
    })
}

fn stopped(evidence: Option<&LocalFundingErrorData>) -> FundingAttemptStoppedData {
    FundingAttemptStoppedData {
        service_code: None,
        transaction_hash: evidence.map(|v| v.tx_hash),
        signed_transaction: evidence.map(|v| v.signed_transaction.clone()),
        // Do not reconstruct a full native receipt from the funding service's
        // legacy result. Callers can query the receipt using the retained hash.
        receipt: None,
    }
}

/// Map existing local funding failures without granting retry permission.
pub fn rpc_error(error: &RpcServerError) -> ErrorObjectOwned {
    rpc_error_with_evidence(error, None)
}

/// Preserve durable signed evidence and a known terminal cause on funding errors.
pub fn rpc_error_with_evidence(
    error: &RpcServerError,
    evidence: Option<&LocalFundingErrorData>,
) -> ErrorObjectOwned {
    if let Some(evidence) = evidence
        && let Err(error) = validate_signed(evidence)
    {
        return rpc_error(&error);
    }
    match error {
        RpcServerError::TxRejected(_) => ErrorObjectOwned::owned(
            codes::FUNDING_ATTEMPT_STOPPED,
            error.to_string(),
            Some(ErrorData::FundingAttemptStopped(stopped(evidence))),
        ),
        RpcServerError::LocalFundingUnresolved {
            evidence, cause, ..
        } => {
            if let Err(error) = validate_signed(evidence) {
                return rpc_error(&error);
            }
            let cause = cause
                .as_deref()
                .filter(|error| matches!(error, RpcServerError::TxRejected(_)))
                .map(|_| FundingCause::FundingAttemptStopped(stopped(Some(evidence))));
            ErrorObjectOwned::owned(
                codes::LOCAL_FUNDING_UNRESOLVED,
                error.to_string(),
                Some(ErrorData::LocalFundingUnresolved {
                    transaction_hash: evidence.tx_hash,
                    signed_transaction: evidence.signed_transaction.clone(),
                    cause,
                }),
            )
        }
        _ => super::read::rpc_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thruput_json_rpc_types::{base58::Base58, hex::HexBytes, transaction::TxHash};
    fn success(return_data: Option<HexBytes>, status: ExecStatus) -> AirdropAttemptResponse {
        AirdropAttemptResponse::Succeeded {
            recipient: Base58([1; 32].into()),
            tx_hash: TxHash(Base58([2; 32].into())),
            raw_amount: "7".into(),
            height: 1.into(),
            status,
            return_data,
        }
    }
    #[test]
    fn succeeded_funding_preserves_observed_output_and_rejects_missing_or_failed_evidence() {
        let address = Base58([1; 32].into());
        let result =
            airdrop_result(address, success(Some(HexBytes(vec![9, 8])), ExecStatus::Ok)).unwrap();
        assert!(
            matches!(result, out::AirdropAttempt::Succeeded { execution: out::AirdropExecution::Return { return_data }, .. } if return_data.0 == [9,8])
        );
        assert!(airdrop_result(address, success(None, ExecStatus::Ok)).is_err());
        assert!(
            airdrop_result(
                address,
                success(Some(HexBytes(vec![])), ExecStatus::NonDeterministicError)
            )
            .is_err()
        );
        assert!(
            airdrop_result(
                Base58([3; 32].into()),
                success(Some(HexBytes(vec![])), ExecStatus::Ok)
            )
            .is_err()
        );
    }
    fn assert_classified(method: &str, error: &ErrorObjectOwned) {
        assert!(matches!(
            thruput_json_rpc_types::error::native::DecodedError::decode(
                method,
                serde_json::to_value(error).unwrap()
            ),
            thruput_json_rpc_types::error::native::DecodedError::Known { .. }
        ));
    }

    #[test]
    fn retained_signed_evidence_uses_canonical_envelope_validation() {
        let mut signed = crate::tx::test_fixtures::canonical_signed_transaction();
        let bytes = xdrs::txs::encode(&signed).unwrap();
        let validated = crate::tx::validate_raw_transaction_bytes(
            bytes.clone(),
            &thruput_tx_validation::NetworkConfigParams::default(),
        )
        .unwrap();
        let evidence = LocalFundingErrorData::new(validated.hash, HexBytes(bytes));
        let error = rpc_error_with_evidence(
            &RpcServerError::TxRejected("stopped".into()),
            Some(&evidence),
        );
        assert_eq!(error.code(), codes::FUNDING_ATTEMPT_STOPPED);
        assert_classified("faucet_requestAirdrop", &error);
        assert_classified("dev_fundAccount", &error);
        signed.pubkey_claim_key = None;
        let invalid = LocalFundingErrorData::new(
            validated.hash,
            HexBytes(xdrs::txs::encode(&signed).unwrap()),
        );
        let error = rpc_error_with_evidence(
            &RpcServerError::TxRejected("stopped".into()),
            Some(&invalid),
        );
        assert_eq!(error.code(), codes::BACKEND_DATA_INVALID);
        assert_classified("faucet_requestAirdrop", &error);
    }

    #[test]
    fn funding_stop_has_no_admission_and_invalid_signed_evidence_fails_closed() {
        let error = rpc_error(&RpcServerError::TxRejected("stopped".into()));
        assert_eq!(error.code(), codes::FUNDING_ATTEMPT_STOPPED);
        assert_classified("faucet_requestAirdrop", &error);
        let value: serde_json::Value = serde_json::from_str(error.data().unwrap().get()).unwrap();
        assert_eq!(value["kind"], "fundingAttemptStopped");
        assert!(value.get("admission").is_none());
        let evidence =
            LocalFundingErrorData::new(TxHash(Base58([2; 32].into())), HexBytes(vec![1]));
        assert_eq!(
            rpc_error_with_evidence(
                &RpcServerError::TxRejected("stopped".into()),
                Some(&evidence)
            )
            .code(),
            codes::BACKEND_DATA_INVALID
        );
    }
}
