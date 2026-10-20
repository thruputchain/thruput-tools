use super::GroundhogRpcServer;
use crate::native::transaction::{
    NativeSubmitPlane, NativeTransactionReadPlane, SubmissionAttempt, read_error, submit_error,
    within_native_read_deadline,
};
use crate::{
    ChainPlane, RpcRequestContext, RpcServerError, ValidatedTransaction,
    tx::validate_raw_transaction,
};
use jsonrpsee::core::RpcResult;
use thruput_json_rpc_types::error::native::BackendDataInvalidReason;
use thruput_json_rpc_types::{requests as input, responses as out, transaction::TxHash};

impl<S, C: ChainPlane, R, V> GroundhogRpcServer<S, C, R, V> {
    pub(super) async fn prepare_transaction(
        &self,
        context: RpcRequestContext,
        tx_hex: &str,
        hash: &mut Option<TxHash>,
    ) -> Result<ValidatedTransaction, RpcServerError> {
        // The pre-checks are advisory mirrors of what the scheduler and
        // executor enforce authoritatively: an unavailable tip reading
        // skips the chain-dependent checks rather than blocking submission.
        let mut tip = self.cached_tip_state(context).await;
        let params = tip
            .state
            .as_ref()
            .and_then(|tip| tip.params)
            .unwrap_or_default();
        let tx = match validate_raw_transaction(tx_hex, &params) {
            Ok(tx) => tx,
            // A cached config can lag an upgrade that raised this
            // bound, so re-read the tip before rejecting. One this
            // request fetched itself is already current.
            Err(RpcServerError::TxInvalidDetailed(_)) if !tip.probed => {
                tip = self.fresh_tip_state(context).await;
                let params = tip
                    .state
                    .as_ref()
                    .and_then(|tip| tip.params)
                    .unwrap_or(params);
                validate_raw_transaction(tx_hex, &params)?
            }
            Err(error) => return Err(error),
        };
        *hash = Some(tx.hash);
        // A transaction can only be included at heights strictly above the
        // current tip, so `expiry <= tip` can never commit.
        if let Some(tip_height) = tip.state.as_ref().map(|tip| tip.height)
            && *tx.block_expiry_height <= tip_height
        {
            return Err(RpcServerError::TxRejected(format!(
                "transaction expired: blockExpiryHeight {} is not past chain tip {}; \
                     re-sign with a later expiry height",
                *tx.block_expiry_height, tip_height
            )));
        }
        Ok(tx)
    }
}

impl<S: NativeSubmitPlane, C: ChainPlane, R, V> GroundhogRpcServer<S, C, R, V> {
    /// Native single-attempt submission. Not registered by production binaries.
    #[tracing::instrument(skip_all)]
    pub async fn native_send_transaction(
        &self,
        request: input::SendTransactionRequest,
    ) -> RpcResult<out::SendTransactionResult> {
        let context = self.request_context();
        let attempt = SubmissionAttempt::default();
        let _cancel_on_drop = attempt.guard();
        let mut hash = None;
        let future = async {
            let tx_hex = thruput_json_rpc_types::hex::prefixed_hex(&request.signed_transaction.0);
            let tx = self
                .prepare_transaction(context, &tx_hex, &mut hash)
                .await?;
            // Synchronous validation can consume the remaining budget between
            // timer polls. Do not start a new submission after it expires.
            if context
                .remaining()
                .is_some_and(|remaining| remaining.is_zero())
            {
                return Err(RpcServerError::BackendTimeout);
            }
            let transaction_hash = tx.hash;
            self.submit
                .native_submit(context, tx, attempt.clone())
                .await?;
            Ok(out::SendTransactionResult {
                transaction_hash,
                admission: out::Admission::Accepted,
            })
        };
        let result = if let Some(deadline) = context.deadline() {
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(deadline.into()) => Err(RpcServerError::BackendTimeout),
                result = future => result,
            }
        } else {
            future.await
        };
        result.map_err(|error| submit_error(&error, &attempt, hash))
    }
}

impl<S, C, R: NativeTransactionReadPlane, V> GroundhogRpcServer<S, C, R, V> {
    /// Native committed receipt lookup, preserving independently validated observation.
    #[tracing::instrument(skip_all)]
    pub async fn native_transaction_receipt(
        &self,
        request: input::TransactionQuery,
    ) -> RpcResult<out::ReceiptResult> {
        let context = self.request_context();
        let hash = request.transaction_hash;
        let result =
            within_native_read_deadline(context, self.read.native_receipt(context, hash)).await;
        match result {
            Ok(Some(receipt)) if receipt.transaction_hash != hash || *receipt.block_height < 0 => {
                Err(read_error(crate::native::transaction::invalid_receipt()))
            }
            Ok(receipt) => Ok(receipt),
            Err(mut error) => {
                if error
                    .observation
                    .as_ref()
                    .is_some_and(|o| o.transaction_hash != hash || *o.block_height < 0)
                {
                    error = crate::native::transaction::invalid_receipt();
                }
                Err(read_error(error))
            }
        }
    }
    /// Native retained status lookup. A negative observation does not prove nonexecution.
    #[tracing::instrument(skip_all)]
    pub async fn native_transaction_status(
        &self,
        request: input::TransactionQuery,
    ) -> RpcResult<out::TransactionStatus> {
        let context = self.request_context();
        let result = within_native_read_deadline(
            context,
            self.read.native_status(context, request.transaction_hash),
        )
        .await;
        match result {
            Ok(out::TransactionStatus::Committed { ref block_height }) if **block_height < 0 => {
                Err(read_error(
                    RpcServerError::invalid_data(
                        BackendDataInvalidReason::InconsistentResult,
                        "committed status has a negative height",
                    )
                    .into(),
                ))
            }
            Ok(out::TransactionStatus::RecentFailure {
                ref attempt_block_height,
                ..
            }) if **attempt_block_height < 0 => Err(read_error(
                RpcServerError::invalid_data(
                    BackendDataInvalidReason::InconsistentResult,
                    "recent failure has a negative attempt height",
                )
                .into(),
            )),
            Ok(status) => Ok(status),
            Err(mut error) => {
                // Receipt observations are only part of the receipt error contract.
                error.observation = None;
                Err(read_error(error))
            }
        }
    }
}

#[cfg(test)]
mod tests;
