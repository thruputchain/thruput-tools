//! The canned hogd-dev x23 dataset: committed transactions, event pages, and
//! storage the fake services serve.
//!
//! The `fake_hogd_dev` executable and the crate's unit-test fakes both include
//! this file with `#[path]`, so the wire answers a fake serves and the values
//! a test asserts come from one definition.

use std::collections::HashMap;

use rpc_driver::ProgramError;
use thruput_indexer::TxInfo;
use thruput_xdr::auth_claim::{AuthClaim, AuthClaim_source_address_claim};
use thruput_xdr::contract::ContractEntrypointID;
use thruput_xdr::event::{IdentifiedEvent, TxEmittedEvent, TxEventLog};
use thruput_xdr::exec_result::ExecResult;
use thruput_xdr::transaction::{
    Invocation, InvocationFrameOutput, NondeterministicProposalData, ProposedTransaction,
    SignedTransaction, Transaction, Uniquifier,
};
use thruput_xdr::types::{BlockHeight, ContractAddress, GasAmount, SHA256Hash};
use xdr_common::{BytesM, VecM};

/// First committed tx hash (all 0xAA bytes).
pub const CANNED_TX_HASH_A: [u8; 32] = [0xAAu8; 32];
/// Second committed tx hash (all 0xBB bytes).
pub const CANNED_TX_HASH_B: [u8; 32] = [0xBBu8; 32];
/// A hash that is *not* in the committed set (all 0xFF bytes).
pub const CANNED_TX_HASH_UNKNOWN: [u8; 32] = [0xFFu8; 32];
/// Block height for `CANNED_TX_HASH_A`.
pub const CANNED_HEIGHT_A: i64 = 5;
/// Block height for `CANNED_TX_HASH_B`, and the latest block history pages report.
pub const CANNED_HEIGHT_B: i64 = 10;
/// Gas the event log records for `CANNED_TX_HASH_A`.
pub const CANNED_GAS_USED_A: u64 = 1_500;
/// Gas the event log records for `CANNED_TX_HASH_B`.
pub const CANNED_GAS_USED_B: u64 = 2_500;
/// Cursor between page 1 and page 2 of the canned event stream.
pub const CANNED_EVENT_CURSOR: &[u8] = b"cursor-page-2";
/// Contract-data key the fake `DbGateway` holds `CANNED_STORAGE_VALUE` under.
pub const CANNED_STORAGE_KEY: [u8; 32] = [0x42u8; 32];
/// Storage value stored under `CANNED_STORAGE_KEY`.
pub const CANNED_STORAGE_VALUE: &[u8] = b"local-node-storage";
/// Snapshot height the fake `DbGateway` reports for storage reads.
pub const CANNED_STORAGE_HEIGHT: i64 = 12;

/// Committed transactions keyed by `TransactionHash` bytes.
pub type CommittedSet = HashMap<[u8; 32], TxInfo>;

/// Sequential event pages, each `(events, next_cursor)`. An empty
/// `next_cursor` marks the last page.
pub type EventPages = Vec<(Vec<IdentifiedEvent>, Vec<u8>)>;

/// A structurally complete transaction that is not valid on-chain; it only
/// needs to round-trip through x23.
pub fn dummy_signed_transaction() -> SignedTransaction {
    SignedTransaction {
        tx: Transaction {
            invocation: Invocation {
                invoked_address: ContractAddress::from([0u8; 32]),
                entrypoint: ContractEntrypointID::from([0u8; 8]),
                gas_limit: GasAmount::from(0u64),
                calldata: BytesM(vec![]),
            },
            uniquifier: Uniquifier {
                block_expiry_height: BlockHeight::from(0i64),
                nonce: 0,
            },
            contracts_to_deploy: VecM(vec![]),
            auth_claim: AuthClaim::kSourceAddressAuthData {
                source_address_claim: AuthClaim_source_address_claim {
                    allowed_keys: 0,
                    required_weight: 0,
                },
            },
        },
        signatures: VecM(vec![]),
        pubkey_claim_key: None,
    }
}

/// [`dummy_signed_transaction`] with empty nondeterministic proposal data,
/// standing in for a `TxInfo.tx` body.
pub fn dummy_proposed_transaction() -> ProposedTransaction {
    ProposedTransaction {
        signed_tx: dummy_signed_transaction(),
        nondeterministic_proposal_data: NondeterministicProposalData {
            nondeterministic_bits: BytesM(vec![]),
            nondeterministic_bit_count: 0,
            observed_queue_entry_sequence: VecM(vec![]),
        },
    }
}

/// `CANNED_TX_HASH_A` and `CANNED_TX_HASH_B`, each committed with an empty
/// successful return and its canned gas.
pub fn committed_set() -> CommittedSet {
    [
        (CANNED_TX_HASH_A, CANNED_HEIGHT_A, CANNED_GAS_USED_A),
        (CANNED_TX_HASH_B, CANNED_HEIGHT_B, CANNED_GAS_USED_B),
    ]
    .into_iter()
    .map(|(hash, height, gas_consumed)| {
        let info = TxInfo {
            height: BlockHeight::from(height),
            tx: dummy_proposed_transaction(),
            events: TxEventLog {
                invoked_addresses: VecM(vec![]),
                tx_hash: SHA256Hash::from(hash),
                exec_result: ExecResult::ExecResultOK,
                root_output: InvocationFrameOutput::Return {
                    return_buffer: BytesM(vec![]),
                },
                gas_consumed,
                events: VecM(vec![]),
            },
        };
        (hash, info)
    })
    .collect()
}

/// Two pages: `CANNED_TX_HASH_A`'s two events (from contract `[1; 32]`) and
/// `CANNED_EVENT_CURSOR`, then `CANNED_TX_HASH_B`'s one event (from contract
/// `[2; 32]`) and an empty cursor.
pub fn event_pages() -> EventPages {
    let event = |tx_hash, block_height, sequence_number, contract| IdentifiedEvent {
        tx_hash: SHA256Hash::from(tx_hash),
        block_height: BlockHeight::from(block_height),
        sequence_number,
        emitted_event: TxEmittedEvent {
            contract_address: ContractAddress::from(contract),
            num_topics: 0,
            serialized_event: BytesM(vec![]),
        },
    };
    vec![
        (
            vec![
                event(CANNED_TX_HASH_A, CANNED_HEIGHT_A, 0, [0x01u8; 32]),
                event(CANNED_TX_HASH_A, CANNED_HEIGHT_A, 1, [0x01u8; 32]),
            ],
            CANNED_EVENT_CURSOR.to_vec(),
        ),
        (
            vec![event(CANNED_TX_HASH_B, CANNED_HEIGHT_B, 0, [0x02u8; 32])],
            vec![],
        ),
    ]
}

/// The page an event query with `cursor` reads: the first page for an empty
/// cursor, otherwise the page after the one that issued `cursor`. A cursor
/// the dataset never issued is an RPC error.
pub fn resolve_event_page<'a>(
    pages: &'a EventPages,
    cursor: &[u8],
) -> Result<&'a (Vec<IdentifiedEvent>, Vec<u8>), ProgramError> {
    if cursor.is_empty() {
        return Ok(&pages[0]);
    }
    pages
        .iter()
        .position(|(_, next)| !next.is_empty() && next.as_slice() == cursor)
        .map(|index| &pages[index + 1])
        .ok_or_else(|| {
            ProgramError::msg("unknown event cursor; fake indexer only handles canned dataset")
        })
}
