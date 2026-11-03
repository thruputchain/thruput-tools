use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thruput_json_rpc_client::native::NativeClient;
use thruput_json_rpc_types::{
    ContractAddress,
    requests::{AddressQuery, ContractArtifactQuery},
    responses::{ContractArtifactResult, ContractDeployment},
};
use thruput_xdr::contract::{Contract, ContractType};

use crate::account_resolution::resolve_contract_target;
use crate::config::{ProfileOverrides, resolve_profile_with_overrides};
use crate::result::CommandResult;
use crate::{CliError, Print};

mod summary;

/// Read deployment metadata and the embedded spec from the selected node.
pub fn run(
    address: &str,
    output: Option<&Path>,
    project_start: &Path,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let profile = resolve_profile_with_overrides(project_start, profile, overrides)?;
    let rpc_url = profile.rpc_endpoint()?;
    let address = ContractAddress::from(resolve_contract_target(&profile, address)?);
    let client = NativeClient::new(crate::http_client::rpc_client(rpc_url)?);
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let data = rt.block_on(async {
        thruput_client::check_endpoint(&client, &["getContractDeployment", "getContractArtifact"])
            .await
            .map_err(|e| crate::native_rpc::rpc_error(&e))?;
        let read = crate::native_rpc::known(
            "getContractDeployment",
            client
                .get_contract_deployment(AddressQuery { address })
                .await,
        )?;
        if read.address != address {
            return Err(CliError::SpecFailed(
                "deployment response address mismatch".into(),
            ));
        }
        let (status, spec) = if let Some(deployment) = &read.deployment {
            let artifact = crate::native_rpc::known(
                "getContractArtifact",
                client
                    .get_contract_artifact(ContractArtifactQuery {
                        hash: deployment.hash,
                    })
                    .await,
            )?;
            inspect_artifact(deployment, &artifact)?
        } else {
            ("notDeployed", Value::Null)
        };
        Ok::<_, CliError>(json!({
            "schemaVersion": 1,
            "address": address.to_string(),
            "stateBlockHeight": read.state_block_height.to_string(),
            "deployment": read.deployment,
            "specStatus": status,
            "spec": spec,
        }))
    })?;
    if let Some(path) = output {
        if data["spec"].is_null() {
            return Err(CliError::SpecFailed(format!(
                "no embedded spec to export ({})",
                data["specStatus"].as_str().unwrap_or_default()
            )));
        }
        let mut contents = serde_json::to_string_pretty(&data["spec"])
            .map_err(|e| CliError::SpecFailed(e.to_string()))?;
        contents.push('\n');
        std::fs::write(path, contents)?;
    }
    if !print.is_json() {
        summary::render(&data, print);
    }
    if let Some(path) = output {
        print.detail("Spec written", path.display());
    }
    print.json(&CommandResult::new(cmd, data))?;
    Ok(())
}

fn inspect_artifact(
    deployment: &ContractDeployment,
    artifact: &ContractArtifactResult,
) -> Result<(&'static str, Value), CliError> {
    let invalid = |message: &str| CliError::SpecFailed(message.into());
    let bytes = &artifact.contract_xdr.0;
    if artifact.hash != deployment.hash
        || Sha256::digest(bytes).as_slice() != deployment.hash.0.as_ref()
    {
        return Err(invalid("contract artifact hash mismatch"));
    }
    let contract: Contract =
        xdrs::txs::decode(bytes).map_err(|_| invalid("invalid Contract XDR"))?;
    if xdrs::txs::encode(&contract).map_err(|_| invalid("cannot encode Contract XDR"))? != *bytes {
        return Err(invalid("noncanonical Contract XDR"));
    }
    match contract {
        Contract::kRv32 { rv32_contract } => {
            if deployment.contract_type != ContractType::kRv32.0
                || usize::try_from(deployment.size).ok() != Some(rv32_contract.data.0.len())
            {
                return Err(invalid("artifact does not match deployment type or size"));
            }
            let bytes = &rv32_contract.data.0;
            if bytes.len() < 12 || bytes[..8] != thruput_rv32_artifact::MAGIC {
                return Err(invalid("invalid RV32 artifact header"));
            }
            if bytes[8..12] != thruput_rv32_artifact::FORMAT_VERSION.to_le_bytes() {
                return Ok(("unsupportedFormat", Value::Null));
            }
            let Some(embedded) = thruput_rv32_artifact::artifact_spec(bytes)
                .map_err(|e| CliError::SpecFailed(e.to_string()))?
            else {
                return Ok(("absent", Value::Null));
            };
            if embedded.schema_version != crate::contract_spec::ARTIFACT_SPEC_SCHEMA_VERSION {
                return Ok(("unsupportedSchema", Value::Null));
            }
            let spec = crate::contract_spec::load_contract_spec_from_xdr(embedded.bytes)?;
            // Preflight already bounds type depth to 64. The human JSON shape
            // adds object/array levels and can exceed serde_json's default 128.
            let json = thruput_spec_tool::render_spec_json(&spec);
            let mut reader = serde_json::Deserializer::from_str(&json);
            reader.disable_recursion_limit();
            let value = <Value as serde::Deserialize>::deserialize(&mut reader)
                .map_err(|e| CliError::SpecFailed(e.to_string()))?;
            Ok(("available", value))
        }
        Contract::kNative { .. }
            if deployment.contract_type == ContractType::kNative.0 && deployment.size == 0 =>
        {
            Ok(("native", Value::Null))
        }
        _ => Err(invalid(
            "unsupported contract type or deployment type mismatch",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thruput_json_rpc_types::{base58::Base58, hex::HexBytes};
    use thruput_rv32_artifact::{ArtifactSpec, build_artifact, build_artifact_with_spec};
    use thruput_xdr::{contract::ContractBlob, types::SHA256Hash};

    fn response(data: Vec<u8>) -> (ContractDeployment, ContractArtifactResult) {
        let size = u32::try_from(data.len()).unwrap();
        let contract = Contract::kRv32 {
            rv32_contract: ContractBlob {
                data: data.try_into().unwrap(),
            },
        };
        let bytes = xdrs::txs::encode(&contract).unwrap();
        let hash = Base58(SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(&bytes))));
        (
            ContractDeployment {
                hash,
                size,
                contract_type: ContractType::kRv32.0,
            },
            ContractArtifactResult {
                hash,
                contract_xdr: HexBytes(bytes),
            },
        )
    }

    #[test]
    fn reads_embedded_spec_from_hash_checked_contract() {
        let spec = thruput_contract_spec::ContractSpec {
            interfaces: vec![].try_into().unwrap(),
            type_defs: vec![].try_into().unwrap(),
            events: vec![].try_into().unwrap(),
        };
        let bytes = xdrs::txs::encode(&spec).unwrap();
        let data = build_artifact_with_spec(
            &[0; 4],
            &[],
            0,
            16,
            Some(ArtifactSpec {
                schema_version: 1,
                bytes: &bytes,
            }),
        )
        .unwrap();
        let (deployment, artifact) = response(data);
        let (status, value) = inspect_artifact(&deployment, &artifact).unwrap();
        assert_eq!(status, "available");
        assert_eq!(
            value,
            serde_json::from_str::<Value>(&thruput_spec_tool::render_spec_json(&spec)).unwrap()
        );
    }

    #[test]
    fn rejects_changed_bytes_and_inconsistent_deployment() {
        let (mut deployment, mut artifact) = response(build_artifact(&[0; 4], &[], 0, 16).unwrap());
        deployment.size += 1;
        assert!(inspect_artifact(&deployment, &artifact).is_err());
        deployment.size -= 1;
        artifact.contract_xdr.0[8] ^= 1;
        assert!(inspect_artifact(&deployment, &artifact).is_err());
    }

    #[test]
    fn distinguishes_absent_spec_from_unsupported_schema() {
        let (deployment, artifact) = response(build_artifact(&[0; 4], &[], 0, 16).unwrap());
        assert_eq!(
            inspect_artifact(&deployment, &artifact).unwrap(),
            ("absent", Value::Null)
        );
        let data = build_artifact_with_spec(
            &[0; 4],
            &[],
            0,
            16,
            Some(ArtifactSpec {
                schema_version: 99,
                bytes: &[1],
            }),
        )
        .unwrap();
        let (deployment, artifact) = response(data);
        assert_eq!(
            inspect_artifact(&deployment, &artifact).unwrap(),
            ("unsupportedSchema", Value::Null)
        );
    }

    #[test]
    fn preserves_identity_for_unknown_formats_but_rejects_bad_magic() {
        let mut data = build_artifact(&[0; 4], &[], 0, 16).unwrap();
        data[8..12].copy_from_slice(&5_u32.to_le_bytes());
        let (deployment, artifact) = response(data.clone());
        assert_eq!(
            inspect_artifact(&deployment, &artifact).unwrap(),
            ("unsupportedFormat", Value::Null)
        );
        data[0] ^= 1;
        let (deployment, artifact) = response(data);
        assert!(inspect_artifact(&deployment, &artifact).is_err());
    }

    #[test]
    fn inspects_the_maximum_supported_tuple_depth() {
        use thruput_contract_spec::ContractTypeKind;
        let mut bytes = Vec::new();
        for word in [1_u32, 0, 0, 0, 1, 0, 0] {
            bytes.extend(word.to_be_bytes());
        }
        for _ in 1..64 {
            bytes.extend(ContractTypeKind::Tuple.0.to_be_bytes());
            bytes.extend(1_u32.to_be_bytes());
        }
        bytes.extend(ContractTypeKind::U8.0.to_be_bytes());
        bytes.extend([0; 12]);
        let data = build_artifact_with_spec(
            &[0; 4],
            &[],
            0,
            16,
            Some(ArtifactSpec {
                schema_version: 1,
                bytes: &bytes,
            }),
        )
        .unwrap();
        let (deployment, artifact) = response(data);
        let (status, value) = inspect_artifact(&deployment, &artifact).unwrap();
        assert_eq!(status, "available");
        assert!(!value["interfaces"].as_array().unwrap().is_empty());
    }

    #[test]
    fn native_identity_does_not_claim_an_embedded_spec() {
        let bytes = xdrs::txs::encode(&Contract::kNative {
            native_contract_id: thruput_xdr::precompiles::HogexecPrecompiledNativeContracts::from(
                0,
            ),
        })
        .unwrap();
        let hash = Base58(SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(&bytes))));
        let deployment = ContractDeployment {
            hash,
            size: 0,
            contract_type: ContractType::kNative.0,
        };
        let artifact = ContractArtifactResult {
            hash,
            contract_xdr: HexBytes(bytes),
        };
        assert_eq!(
            inspect_artifact(&deployment, &artifact).unwrap(),
            ("native", Value::Null)
        );
    }

    #[test]
    fn matching_hash_does_not_accept_trailing_xdr_bytes() {
        let (mut deployment, mut artifact) = response(build_artifact(&[0; 4], &[], 0, 16).unwrap());
        artifact.contract_xdr.0.extend_from_slice(&[0; 4]);
        artifact.hash = Base58(SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(
            &artifact.contract_xdr.0,
        ))));
        deployment.hash = artifact.hash;
        assert!(inspect_artifact(&deployment, &artifact).is_err());
    }
}
