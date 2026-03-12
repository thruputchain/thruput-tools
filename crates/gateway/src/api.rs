// TODO: uncomment once groundhog-xdr and jsonrpsee are available
//
// use bytes::Bytes;
// use jsonrpsee::proc_macros::rpc;
// use groundhog_rpc_api_types::{
//     block::{BlockInfo, ChainTip},
//     events::IdentifiedEvent,
//     receipt::TransactionReceipt,
//     transaction::{TransactionStatus, TxHash},
//     BlockHeight, ContractAddress,
// };
//
// /// Phase 1: Core API — submit, query, poll.
// /// Phase 2 adds subscription methods (commented below).
// #[rpc(client, server, namespace = "groundhog")]
// pub trait GroundhogApi {
//     /// Submit a signed transaction. Returns the canonical tx hash.
//     #[method(name = "sendRawTransaction")]
//     async fn send_raw_transaction(&self, tx_bytes: Bytes) -> RpcResult<TxHash>;
//
//     /// Full receipt for a committed transaction.
//     #[method(name = "getTransactionReceipt")]
//     async fn get_transaction_receipt(
//         &self,
//         tx_hash: TxHash,
//     ) -> RpcResult<Option<TransactionReceipt>>;
//
//     /// Lightweight status check (mempool → indexer → unknown).
//     #[method(name = "getTransactionStatus")]
//     async fn get_transaction_status(
//         &self,
//         tx_hash: TxHash,
//     ) -> RpcResult<TransactionStatus>;
//
//     /// Current chain tip: height + block hash.
//     #[method(name = "chainTip")]
//     async fn chain_tip(&self) -> RpcResult<ChainTip>;
//
//     /// Block data at a given height.
//     #[method(name = "getBlockByNumber")]
//     async fn get_block_by_number(
//         &self,
//         height: BlockHeight,
//     ) -> RpcResult<Option<BlockInfo>>;
//
//     /// Number of transactions in the mempool.
//     #[method(name = "pendingTransactionCount")]
//     async fn pending_transaction_count(&self) -> RpcResult<u64>;
//
//     /// Contract events filtered by address and height range.
//     #[method(name = "getContractEvents")]
//     async fn get_contract_events(
//         &self,
//         address: ContractAddress,
//         from_height: BlockHeight,
//         to_height: BlockHeight,
//     ) -> RpcResult<Vec<IdentifiedEvent>>;
//
//     // --- Phase 2: Subscriptions (WebSocket) ---
//
//     // #[subscription(
//     //     name = "subscribeNewBlocks" => "newBlock",
//     //     unsubscribe = "unsubscribeNewBlocks",
//     //     item = BlockInfo,
//     // )]
//     // async fn subscribe_new_blocks(&self);
//     //
//     // #[subscription(
//     //     name = "submitAndWatch" => "txStatus",
//     //     unsubscribe = "unwatchTx",
//     //     item = TransactionStatus,
//     // )]
//     // async fn submit_and_watch(&self, tx_bytes: Bytes);
// }
