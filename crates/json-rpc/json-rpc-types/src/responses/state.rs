use super::{
    Deserialize, Deserializer, JsonSchema, ResponseData, Serialize, UnknownVariant, Value,
    fields::{
        amount_schema, height_schema, hex_schema, i32_schema, nullable, nullable_schema,
        read_amount, read_height, read_hex, u32_schema,
    },
};
use crate::{BlockHeight, ContractAddress, Ed25519PublicKey, SHA256Hash, hex::HexBytes};

response_object! {
/// Deployment at a committed snapshot. Null means no deployment in this snapshot.
ContractDeploymentResult {
    address: ContractAddress,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    state_block_height: BlockHeight,
    #[serde(deserialize_with = "nullable")]
    #[schemars(schema_with = "nullable_schema::<ContractDeployment>")]
    deployment: Option<ContractDeployment>,
}
}

response_object! {
/// Stored deployment identity. Size is the artifact byte count, or zero for native code.
ContractDeployment {
    hash: SHA256Hash,
    #[schemars(schema_with = "u32_schema")]
    size: u32,
    #[schemars(schema_with = "i32_schema")]
    contract_type: i32,
}
}

response_object! {
/// Complete canonical XDR Contract bytes, including discriminant and padding.
/// Content retrieval does not prove deployment or transaction commitment.
ContractArtifactResult {
    hash: SHA256Hash,
    #[serde(deserialize_with = "read_hex")]
    #[schemars(schema_with = "hex_schema")]
    contract_xdr: HexBytes,
}
}

response_object! {
/// Storage at a committed snapshot. The item member is required, even if null.
StorageResult {
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    state_block_height: BlockHeight,
    #[serde(deserialize_with = "nullable")]
    #[schemars(schema_with = "nullable_schema::<StorageItem>")]
    item: Option<StorageItem>,
}
}

response_enum! {
/// Known storage projections. Unsupported variants never imply item absence.
StorageItem, "kind" {
    NonnegativeInt("nonnegativeInt", true) {
        #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_amount")]
        #[schemars(schema_with = "amount_schema")]
        value: i128,
    },
    Bytestring("bytestring", true) {
        #[serde(deserialize_with = "read_hex")]
        #[schemars(schema_with = "hex_schema")]
        bytes: HexBytes,
    },
    AuthData("authData", true) {},
    RetrocausalQueue("retrocausalQueue", true) {},
    ReplayCacheEntry("replayCacheEntry", true) {},
    DeployedContract("deployedContract", true) {
        hash: SHA256Hash,
        #[schemars(schema_with = "u32_schema")]
        size: u32,
        #[schemars(schema_with = "i32_schema")]
        contract_type: i32,
    },
    Unknown("unknown", false) {
        #[schemars(schema_with = "i32_schema")]
        type_id: i32,
    },
}
}

response_object! {
/// Free native-token base units; storage collateral is separate.
BalanceResult {
    address: ContractAddress,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    state_block_height: BlockHeight,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_amount")]
    #[schemars(schema_with = "amount_schema")]
    raw_balance: i128,
}
}

response_object! {
/// Registered signers at a snapshot. Null does not imply account absence and
/// does not recover the key or scheme of a pubkey-derived account.
RegisteredAuthResult {
    address: ContractAddress,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    state_block_height: BlockHeight,
    #[serde(deserialize_with = "nullable")]
    #[schemars(schema_with = "nullable_schema::<AuthConfiguration>")]
    configuration: Option<AuthConfiguration>,
}
}

response_object! {
/// Registered signer slots and threshold. Slot IDs need not be contiguous.
AuthConfiguration {
    signers: Vec<AuthSigner>,
    #[schemars(schema_with = "u32_schema")]
    required_weight: u32,
}
}

response_object! {
/// One registered signer; its key ID is not its array position.
AuthSigner {
    #[schemars(schema_with = "u32_schema")]
    key_id: u32,
    public_key: PublicKey,
    #[schemars(schema_with = "u32_schema")]
    max_weight: u32,
}
}

response_enum! {
/// Implemented registered-key projection. Pubkey PQ transactions do not imply
/// registered PQ support; an unknown scheme is unusable by a signer.
PublicKey, "scheme" {
    Ed25519("ed25519", true) { key: Ed25519PublicKey },
    Unknown("unknown", false) {
        #[schemars(schema_with = "i32_schema")]
        scheme_id: i32,
    },
}
}
