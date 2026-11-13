//! The network identity and wallet entrypoints a signature is bound to.
//!
//! Every profile pins its `network_hash` (or takes it from a local runtime
//! manifest). The pin is the only identity a command signs for; the RPC
//! endpoint is checked against it, never trusted to supply it.

use thruput_json_rpc_client::native::NativeClient;
use thruput_signing::WALLET_DEPLOY_ENTRYPOINT;
use thruput_xdr::contract::ContractEntrypointID;

use crate::CliError;
use crate::config::ResolvedProfile;

pub(crate) const WALLET_ENTRYPOINT_SOURCE_LEGACY_DEFAULT: &str = "legacy_default";

// Frozen compatibility selector for saved-wallet profiles, which sign through
// the canonical deployed-wallet ABI rather than the native account.
const LEGACY_WALLET_DO_ACTION_ENTRYPOINT: [u8; 8] = 0x1984_6D75_0FAD_81D6_u64.to_be_bytes();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WalletEntrypoints {
    pub(crate) deploy: ContractEntrypointID,
    pub(crate) do_action: ContractEntrypointID,
    pub(crate) source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WalletSigningContext {
    pub(crate) entrypoints: WalletEntrypoints,
    pub(crate) network_hash: thruput_xdr::types::NetworkHash,
}

/// Signing context for the profile's pinned network, after checking that its
/// RPC endpoint serves that network.
///
/// `native` selects the native account entrypoints; a saved legacy wallet
/// binding signs through the frozen deployed-wallet ABI instead.
pub(crate) fn resolve_signing_context(
    profile: &ResolvedProfile,
    pinned: &thruput_xdr::types::NetworkHash,
    native: bool,
    runtime: &tokio::runtime::Runtime,
) -> Result<WalletSigningContext, CliError> {
    let rpc_url = profile.rpc_endpoint()?;
    runtime.block_on(async {
        let client = NativeClient::new(crate::http_client::rpc_client(rpc_url)?);
        thruput_client::check_endpoint(&client, &["getNetworkInfo"])
            .await
            .map_err(|error| crate::native_rpc::rpc_error(&error))?;
        thruput_client::validate_network(&client, pinned)
            .await
            .map_err(|error| crate::native_rpc::rpc_error(&error))
    })?;
    Ok(pinned_signing_context(pinned, native))
}

/// Signing context for the pinned network without asking the RPC endpoint,
/// for a caller that checks the endpoint against the pin itself.
pub(crate) fn pinned_signing_context(
    pinned: &thruput_xdr::types::NetworkHash,
    native: bool,
) -> WalletSigningContext {
    WalletSigningContext {
        network_hash: pinned.clone(),
        entrypoints: if native {
            native_entrypoints()
        } else {
            legacy_entrypoints()
        },
    }
}

fn native_entrypoints() -> WalletEntrypoints {
    WalletEntrypoints {
        deploy: thruput_signing::default_wallet::DEFAULT_WALLET_DEPLOY_ENTRYPOINT,
        do_action: thruput_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT,
        source: "native_default_wallet",
    }
}

fn legacy_entrypoints() -> WalletEntrypoints {
    WalletEntrypoints {
        deploy: ContractEntrypointID::from(WALLET_DEPLOY_ENTRYPOINT),
        do_action: ContractEntrypointID::from(LEGACY_WALLET_DO_ACTION_ENTRYPOINT),
        source: WALLET_ENTRYPOINT_SOURCE_LEGACY_DEFAULT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_default_entrypoints_remain_frozen() {
        let entrypoints = legacy_entrypoints();
        assert_eq!(
            entrypoints.deploy,
            ContractEntrypointID::from(0xADAC_4982_2214_8E4F_u64.to_be_bytes())
        );
        assert_eq!(
            entrypoints.do_action,
            ContractEntrypointID::from(0x1984_6D75_0FAD_81D6_u64.to_be_bytes())
        );
        assert_eq!(entrypoints.source, WALLET_ENTRYPOINT_SOURCE_LEGACY_DEFAULT);
    }
}
