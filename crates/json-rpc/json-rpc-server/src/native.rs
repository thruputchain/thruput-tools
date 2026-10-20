//! Explicit native transport registration, separate from production wire 13.

use std::sync::{Arc, OnceLock};

use jsonrpsee::{RpcModule, core::RegisterMethodError};
use thruput_json_rpc_api::native::NativeApi;
use tracing::Instrument;

/// Native registration under construction. Remove unconfigured methods before
/// finalization. This does not select backends or enable a public endpoint.
pub struct NativeModule<S> {
    module: RpcModule<S>,
    names: Arc<OnceLock<Vec<String>>>,
}

impl<S> std::fmt::Debug for NativeModule<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeModule")
            .field("methods", &self.module.method_names().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

macro_rules! parse {
    (empty, $request:ty, $params:expr) => {
        crate::params::parse_empty($params)
    };
    (named, $request:ty, $params:expr) => {
        crate::params::parse_named::<$request>($params)
    };
}

macro_rules! decorate {
    (plain, $result:expr, $names:expr) => {{
        let _ = $names;
        $result
    }};
    (version, $result:expr, $names:expr) => {{
        let mut result = $result;
        result.supported_methods = $names
            .get()
            .expect("bug: native module served before finalization")
            .clone();
        result
    }};
}

impl<S: NativeApi> NativeModule<S> {
    /// Register the typed core and optional methods. No method is usable until
    /// the caller finalizes this builder and starts a transport explicitly.
    pub fn new(service: S) -> Result<Self, RegisterMethodError> {
        let mut module = RpcModule::new(service);
        let names = Arc::new(OnceLock::<Vec<String>>::new());
        macro_rules! register {
            ($( $(#[$doc:meta])* $name:ident($wire:literal, $parse:ident, $decorate:ident): $request:ty => $response:ty; )*) => {
                $({
                    let names = names.clone();
                    module.register_async_method($wire, move |params, service, _| {
                        let names = names.clone();
                        async move {
                            let request = parse!($parse, $request, &params)?;
                            let result = service.$name(request).await?;
                            Ok::<$response, jsonrpsee::types::ErrorObjectOwned>(decorate!($decorate, result, names))
                        }.instrument(tracing::info_span!("native_rpc", method = $wire))
                    })?;
                })*
            };
        }
        thruput_json_rpc_api::native_methods!(register);
        Ok(Self { module, names })
    }

    /// Remove an unconfigured method. Returns whether it was registered.
    /// Transient backend outages must not change configured capabilities.
    pub fn remove_method(&mut self, name: &'static str) -> bool {
        self.module.remove_method(name).is_some()
    }

    /// Freeze the sorted capability list from the actual remaining methods.
    /// `getVersionInfo` uses this list even if the service supplied another list.
    pub fn finalize(self) -> FinalizedNativeModule<S> {
        let mut names: Vec<_> = self.module.method_names().map(str::to_owned).collect();
        names.sort_unstable();
        self.names
            .set(names.clone())
            .expect("bug: native module finalized twice");
        FinalizedNativeModule {
            module: self.module,
            names,
        }
    }
}

/// Final registration and its discovery list. The extracted module is intended
/// to be passed directly to the transport; later registration mutations would
/// invalidate discovery and require rebuilding through [`NativeModule`].
pub struct FinalizedNativeModule<S> {
    module: RpcModule<S>,
    names: Vec<String>,
}

impl<S> std::fmt::Debug for FinalizedNativeModule<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FinalizedNativeModule")
            .field("supported_methods", &self.names)
            .finish_non_exhaustive()
    }
}

impl<S> FinalizedNativeModule<S> {
    /// Sorted names that will be reported by `getVersionInfo`.
    pub fn supported_methods(&self) -> &[String] {
        &self.names
    }

    /// Transfer the completed registration to a JSON-RPC transport.
    pub fn into_rpc(self) -> RpcModule<S> {
        self.module
    }
}

/// Native read-only backend semantics; production activation is separate.
pub mod read;

/// Native transaction producers and evidence, not registered by production binaries.
pub mod transaction;

/// Composition of implemented native handlers without production activation.
pub mod service;
