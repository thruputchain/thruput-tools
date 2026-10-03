//! Durable, chain-bound evidence retained before local funding broadcasts.

use groundhog_json_rpc_server::RpcServerError;
use groundhog_json_rpc_types::{
    ContractAddress, NetworkHashHex, airdrop::FundAccountRequest, hex::HexBytes,
    transaction::TxHash,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{
    NodeSupervisorError,
    local_state::{LOCAL_FUNDING_ATTEMPTS_FILE, atomic_private_write, private_file, state_error},
};

const MAX_JOURNAL_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RetainedFundingTransaction {
    pub(crate) tx_hash: TxHash,
    pub(crate) signed_transaction: HexBytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FundingRecord {
    pub(crate) request: FundAccountRequest,
    pub(crate) signed: Option<RetainedFundingTransaction>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JournalData {
    schema_version: u32,
    network_hash: NetworkHashHex,
    faucet: ContractAddress,
    records: Vec<FundingRecord>,
}

#[derive(Debug)]
pub(crate) struct FundingJournal {
    path: Option<PathBuf>,
    data: JournalData,
    dirty: bool,
    max_bytes: usize,
}

impl FundingJournal {
    pub(crate) fn memory(network_hash: NetworkHashHex, faucet: ContractAddress) -> Self {
        Self {
            path: None,
            dirty: false,
            max_bytes: MAX_JOURNAL_BYTES,
            data: JournalData {
                schema_version: 1,
                network_hash,
                faucet,
                records: Vec::new(),
            },
        }
    }

    pub(crate) fn open(
        dir: &Path,
        network_hash: NetworkHashHex,
        faucet: ContractAddress,
        fresh: bool,
        signer: &std::sync::Arc<groundhog_signing::LocalSigner>,
    ) -> Result<Self, NodeSupervisorError> {
        let path = dir.join(LOCAL_FUNDING_ATTEMPTS_FILE);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && fresh => {
                let mut journal = Self::memory(network_hash, faucet);
                let bytes = serde_json::to_vec(&journal.data).map_err(|e| state_error(&path, e))?;
                atomic_private_write(&path, &bytes, false).map_err(|e| state_error(&path, e))?;
                journal.path = Some(path);
                Ok(journal)
            }
            Err(error) => Err(state_error(&path, error)),
            Ok(_) => {
                let file = private_file(&path)?;
                let mut bytes = Vec::new();
                file.take((MAX_JOURNAL_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(|e| state_error(&path, e))?;
                if bytes.len() > MAX_JOURNAL_BYTES {
                    return Err(state_error(&path, "funding journal exceeds size limit"));
                }
                let data: JournalData =
                    serde_json::from_slice(&bytes).map_err(|e| state_error(&path, e))?;
                if data.schema_version != 1
                    || data.network_hash != network_hash
                    || data.faucet != faucet
                {
                    return Err(state_error(
                        &path,
                        "funding journal belongs to a different chain",
                    ));
                }
                if fresh && !data.records.is_empty() {
                    return Err(state_error(
                        &path,
                        "funding requests exist without the chain's public bootstrap metadata",
                    ));
                }
                let mut requests = std::collections::HashSet::new();
                for record in &data.records {
                    let request = &record.request;
                    if request.faucet != faucet
                        || !requests.insert((request.recipient, request.client_nonce.clone()))
                    {
                        return Err(state_error(
                            &path,
                            "funding journal has conflicting request identities",
                        ));
                    }
                    if let Some(signed) = &record.signed {
                        crate::airdrop::validate_retained_funding(
                            request,
                            signed,
                            network_hash.to_network_hash(),
                            signer.clone(),
                        )
                        .map_err(|reason| state_error(&path, reason))?;
                    }
                }
                Ok(Self {
                    path: Some(path),
                    data,
                    dirty: false,
                    max_bytes: MAX_JOURNAL_BYTES,
                })
            }
        }
    }

    pub(crate) fn records(&self) -> &[FundingRecord] {
        &self.data.records
    }

    /// A record remains for the chain's lifetime, including completed transfers.
    pub(crate) fn register(
        &mut self,
        request: FundAccountRequest,
    ) -> Result<FundingRecord, RpcServerError> {
        let previous_dirty = self.dirty;
        let position = self.data.records.iter().position(|record| {
            record.request.recipient == request.recipient
                && record.request.client_nonce == request.client_nonce
        });
        let record = if let Some(position) = position {
            let record = &self.data.records[position];
            if record.request != request {
                return Err(RpcServerError::InvalidParam(
                    "funding nonce is already bound to a different amount or chain".into(),
                ));
            }
            record.clone()
        } else {
            let record = FundingRecord {
                request,
                signed: None,
            };
            self.data.records.push(record.clone());
            self.dirty = true;
            record
        };
        if let Err(error) = self.persist() {
            // A size rejection happens before any write. Keep existing evidence
            // usable, but retain mutations after I/O failures with an uncertain rename.
            if position.is_none() && matches!(error, RpcServerError::Overloaded(_)) {
                self.data.records.pop();
                self.dirty = previous_dirty;
            }
            return Err(error);
        }
        Ok(record)
    }

    pub(crate) fn retain_signed(
        &mut self,
        request: &FundAccountRequest,
        signed: RetainedFundingTransaction,
    ) -> Result<(), RpcServerError> {
        let previous_dirty = self.dirty;
        let position = self
            .data
            .records
            .iter()
            .position(|record| record.request == *request)
            .ok_or_else(|| {
                RpcServerError::BackendUnavailable("funding request was not registered".into())
            })?;
        let record = &mut self.data.records[position];
        let newly_signed = record.signed.is_none();
        if let Some(retained) = &record.signed {
            if retained.tx_hash != signed.tx_hash
                || retained.signed_transaction != signed.signed_transaction
            {
                return Err(RpcServerError::BackendUnavailable(
                    "funding request must retain its original signed transaction".into(),
                ));
            }
        } else {
            record.signed = Some(signed);
            self.dirty = true;
        }
        if let Err(error) = self.persist() {
            if newly_signed && matches!(error, RpcServerError::Overloaded(_)) {
                self.data.records[position].signed = None;
                self.dirty = previous_dirty;
            }
            return Err(error);
        }
        Ok(())
    }

    fn persist(&mut self) -> Result<(), RpcServerError> {
        if !self.dirty {
            return Ok(());
        }
        let Some(path) = &self.path else {
            self.dirty = false;
            return Ok(());
        };
        let bytes = serde_json::to_vec(&self.data).map_err(|_| {
            RpcServerError::BackendUnavailable("could not encode funding journal".into())
        })?;
        if bytes.len() > self.max_bytes {
            return Err(RpcServerError::Overloaded(
                "funding journal size limit reached".into(),
            ));
        }
        atomic_private_write(path, &bytes, true).map_err(|e| {
            RpcServerError::BackendUnavailable(format!("could not persist funding journal: {e}"))
        })?;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundhog_json_rpc_types::{airdrop::ClientNonce, hex::Hex0x};

    #[test]
    fn size_rejection_preserves_prior_records_and_dirty_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = FundingJournal::memory([0; 32].into(), Hex0x([0; 32].into()));
        journal.path = Some(dir.path().join("journal.json"));
        let request = FundAccountRequest {
            recipient: Hex0x([1; 32].into()),
            faucet: Hex0x([0; 32].into()),
            raw_amount: "42".into(),
            client_nonce: ClientNonce::new(vec![1]).unwrap(),
        };
        journal.register(request.clone()).unwrap();
        let saved = fs::read(journal.path.as_ref().unwrap()).unwrap();
        journal.max_bytes = saved.len();
        let second = FundAccountRequest {
            client_nonce: ClientNonce::new(vec![2]).unwrap(),
            ..request.clone()
        };
        assert!(matches!(
            journal.register(second),
            Err(RpcServerError::Overloaded(_))
        ));
        assert_eq!(journal.records().len(), 1);
        assert!(!journal.dirty);
        assert!(matches!(
            journal.retain_signed(
                &request,
                RetainedFundingTransaction {
                    tx_hash: TxHash(Hex0x([2; 32].into())),
                    signed_transaction: HexBytes(vec![3; 100]),
                }
            ),
            Err(RpcServerError::Overloaded(_))
        ));
        assert!(journal.records()[0].signed.is_none());
        assert!(!journal.dirty);
        journal.register(request.clone()).unwrap();
        assert_eq!(fs::read(journal.path.as_ref().unwrap()).unwrap(), saved);
        // An earlier uncertain write must still need persistence after rollback.
        journal.dirty = true;
        let second = FundAccountRequest {
            client_nonce: ClientNonce::new(vec![2]).unwrap(),
            ..request
        };
        assert!(journal.register(second).is_err());
        assert!(journal.dirty);
    }
}
