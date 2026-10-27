use async_trait::async_trait;
use aws_config::{BehaviorVersion, Region};
use aws_sdk_secretsmanager::{Client, operation::get_secret_value::GetSecretValueOutput};
use serde::Deserialize;
use zeroize::Zeroizing;

/// Configured source for the faucet signing key.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FaucetSecretKeySource {
    /// Read the hex-encoded key from an environment variable.
    Environment {
        /// Environment variable containing the key.
        variable: String,
    },
    /// Read the hex-encoded key from an AWS Secrets Manager `SecretString`.
    AwsSecretsManager {
        /// Secret name or ARN passed to `GetSecretValue`.
        secret_id: String,
        /// Immutable secret version passed to `GetSecretValue`.
        version_id: String,
        /// AWS region containing the secret.
        region: String,
    },
}

/// Loads the configured faucet signing key without exposing its source to callers.
#[async_trait]
pub trait FaucetSecretKeyLoader: Send + Sync {
    /// Load and validate a 32-byte Ed25519 secret key.
    async fn load(
        &self,
        source: &FaucetSecretKeySource,
    ) -> Result<Zeroizing<[u8; 32]>, FaucetSecretKeyLoadError>;
}

/// Production loader for environment variables and AWS Secrets Manager.
#[derive(Debug, Default)]
pub struct DefaultFaucetSecretKeyLoader;

#[async_trait]
impl FaucetSecretKeyLoader for DefaultFaucetSecretKeyLoader {
    async fn load(
        &self,
        source: &FaucetSecretKeySource,
    ) -> Result<Zeroizing<[u8; 32]>, FaucetSecretKeyLoadError> {
        match source {
            FaucetSecretKeySource::Environment { variable } => load_environment_secret(variable),
            FaucetSecretKeySource::AwsSecretsManager {
                secret_id,
                version_id,
                region,
            } => load_aws_secret(secret_id, version_id, region).await,
        }
    }
}

/// Safe startup errors returned while loading the faucet signing key.
#[derive(Debug, thiserror::Error)]
pub enum FaucetSecretKeyLoadError {
    /// The configured environment variable name was empty.
    #[error("faucet secret-key environment variable name must not be empty")]
    EmptyEnvironmentVariable,
    /// The configured environment variable was absent.
    #[error("faucet secret-key environment variable `{variable}` is not set")]
    MissingEnvironmentVariable { variable: String },
    /// The configured environment variable was not valid UTF-8.
    #[error("faucet secret-key environment variable `{variable}` is not valid UTF-8")]
    InvalidEnvironmentVariable { variable: String },
    /// The configured AWS secret ID or ARN was empty.
    #[error("faucet AWS Secrets Manager secret ID must not be empty")]
    EmptyAwsSecretId,
    /// The configured AWS secret version ID was empty.
    #[error("faucet AWS Secrets Manager version ID must not be empty")]
    EmptyAwsVersionId,
    /// The configured AWS region was empty.
    #[error("faucet AWS Secrets Manager region must not be empty")]
    EmptyAwsRegion,
    /// AWS Secrets Manager could not return the configured secret.
    #[error(
        "failed to load faucet signing key from AWS Secrets Manager secret `{secret_id}` in `{region}`: {message}"
    )]
    AwsGetSecretValue {
        secret_id: String,
        region: String,
        message: String,
    },
    /// The AWS secret did not contain the required string payload.
    #[error(
        "AWS Secrets Manager secret `{secret_id}` must contain a SecretString with a hex-encoded 32-byte key"
    )]
    MissingAwsSecretString { secret_id: String },
    /// AWS returned a different version than requested.
    #[error(
        "AWS Secrets Manager secret `{secret_id}` returned version `{actual_version_id}`, expected `{expected_version_id}`"
    )]
    AwsVersionMismatch {
        secret_id: String,
        expected_version_id: String,
        actual_version_id: String,
    },
    /// The selected source contained a malformed key without exposing the value.
    #[error("faucet signing key from {key_source} must be a hex-encoded 32-byte value")]
    InvalidSecretKey { key_source: String },
}

fn load_environment_secret(
    variable: &str,
) -> Result<Zeroizing<[u8; 32]>, FaucetSecretKeyLoadError> {
    if variable.trim().is_empty() {
        return Err(FaucetSecretKeyLoadError::EmptyEnvironmentVariable);
    }
    let value = std::env::var(variable).map_err(|error| match error {
        std::env::VarError::NotPresent => FaucetSecretKeyLoadError::MissingEnvironmentVariable {
            variable: variable.to_owned(),
        },
        std::env::VarError::NotUnicode(_) => FaucetSecretKeyLoadError::InvalidEnvironmentVariable {
            variable: variable.to_owned(),
        },
    })?;
    let value = Zeroizing::new(value);
    parse_secret_key(&value, format!("environment variable `{variable}`"))
}

async fn load_aws_secret(
    secret_id: &str,
    version_id: &str,
    region: &str,
) -> Result<Zeroizing<[u8; 32]>, FaucetSecretKeyLoadError> {
    if secret_id.trim().is_empty() {
        return Err(FaucetSecretKeyLoadError::EmptyAwsSecretId);
    }
    if region.trim().is_empty() {
        return Err(FaucetSecretKeyLoadError::EmptyAwsRegion);
    }
    if version_id.trim().is_empty() {
        return Err(FaucetSecretKeyLoadError::EmptyAwsVersionId);
    }

    let sdk_config = aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(region.to_owned()))
        .load()
        .await;
    let client = Client::new(&sdk_config);
    let output = request_aws_secret(&client, secret_id, version_id)
        .await
        .map_err(|message| FaucetSecretKeyLoadError::AwsGetSecretValue {
            secret_id: secret_id.to_owned(),
            region: region.to_owned(),
            message,
        })?;
    parse_aws_secret_output(output, secret_id, version_id)
}

async fn request_aws_secret(
    client: &Client,
    secret_id: &str,
    version_id: &str,
) -> Result<GetSecretValueOutput, String> {
    client
        .get_secret_value()
        .secret_id(secret_id)
        .version_id(version_id)
        .send()
        .await
        .map_err(|error| error.to_string())
}

fn parse_aws_secret_output(
    mut output: GetSecretValueOutput,
    secret_id: &str,
    expected_version_id: &str,
) -> Result<Zeroizing<[u8; 32]>, FaucetSecretKeyLoadError> {
    let secret_string = output.secret_string.take().map(Zeroizing::new);
    let _secret_binary = output
        .secret_binary
        .take()
        .map(|value| Zeroizing::new(value.into_inner()));
    let actual_version_id = output.version_id.as_deref().unwrap_or_default();
    if actual_version_id != expected_version_id {
        return Err(FaucetSecretKeyLoadError::AwsVersionMismatch {
            secret_id: secret_id.to_owned(),
            expected_version_id: expected_version_id.to_owned(),
            actual_version_id: actual_version_id.to_owned(),
        });
    }
    let Some(value) = secret_string else {
        return Err(FaucetSecretKeyLoadError::MissingAwsSecretString {
            secret_id: secret_id.to_owned(),
        });
    };
    parse_secret_key(&value, format!("AWS secret `{secret_id}`"))
}

fn parse_secret_key(
    value: &str,
    key_source: String,
) -> Result<Zeroizing<[u8; 32]>, FaucetSecretKeyLoadError> {
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if value.len() != 64 {
        return Err(FaucetSecretKeyLoadError::InvalidSecretKey { key_source });
    }
    let mut key = Zeroizing::new([0_u8; 32]);
    hex::decode_to_slice(value, key.as_mut())
        .map_err(|_| FaucetSecretKeyLoadError::InvalidSecretKey { key_source })?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use aws_sdk_secretsmanager::{
        Client,
        config::{Credentials, Region},
        operation::get_secret_value::GetSecretValueOutput,
        primitives::Blob,
    };
    use aws_smithy_http_client::test_util::capture_request;
    use aws_smithy_types::body::SdkBody;
    use serial_test::serial;

    use super::{
        DefaultFaucetSecretKeyLoader, FaucetSecretKeyLoader, FaucetSecretKeySource,
        parse_aws_secret_output, request_aws_secret,
    };

    const TEST_KEY_VARIABLE: &str = "THRUPUT_FAUCET_TEST_SECRET_KEY";

    #[tokio::test]
    #[serial]
    async fn environment_source_loads_hex_key() {
        unsafe { std::env::set_var(TEST_KEY_VARIABLE, format!("0x{}", "ab".repeat(32))) };
        let source = FaucetSecretKeySource::Environment {
            variable: TEST_KEY_VARIABLE.into(),
        };

        let key = DefaultFaucetSecretKeyLoader
            .load(&source)
            .await
            .expect("bug: valid environment key should load");

        unsafe { std::env::remove_var(TEST_KEY_VARIABLE) };
        assert_eq!(key.as_ref(), &[0xAB; 32]);
    }

    #[tokio::test]
    #[serial]
    async fn environment_source_accepts_uppercase_hex_prefix() {
        unsafe { std::env::set_var(TEST_KEY_VARIABLE, format!("0X{}", "ab".repeat(32))) };
        let source = FaucetSecretKeySource::Environment {
            variable: TEST_KEY_VARIABLE.into(),
        };

        let key = DefaultFaucetSecretKeyLoader
            .load(&source)
            .await
            .expect("bug: uppercase-prefixed environment key should load");

        unsafe { std::env::remove_var(TEST_KEY_VARIABLE) };
        assert_eq!(key.as_ref(), &[0xAB; 32]);
    }

    #[tokio::test]
    #[serial]
    async fn environment_source_fails_closed_when_missing() {
        unsafe { std::env::remove_var(TEST_KEY_VARIABLE) };
        let source = FaucetSecretKeySource::Environment {
            variable: TEST_KEY_VARIABLE.into(),
        };

        let error = DefaultFaucetSecretKeyLoader
            .load(&source)
            .await
            .expect_err("bug: missing environment key should fail");

        assert!(error.to_string().contains(TEST_KEY_VARIABLE), "{error}");
    }

    #[tokio::test]
    #[serial]
    async fn environment_source_error_does_not_expose_malformed_value() {
        let secret_value = "not-a-private-key";
        unsafe { std::env::set_var(TEST_KEY_VARIABLE, secret_value) };
        let source = FaucetSecretKeySource::Environment {
            variable: TEST_KEY_VARIABLE.into(),
        };

        let error = DefaultFaucetSecretKeyLoader
            .load(&source)
            .await
            .expect_err("bug: malformed environment key should fail");

        unsafe { std::env::remove_var(TEST_KEY_VARIABLE) };
        let message = error.to_string();
        assert!(message.contains(TEST_KEY_VARIABLE), "{message}");
        assert!(!message.contains(secret_value), "{message}");
    }

    #[tokio::test]
    async fn aws_source_validates_config_before_loading_sdk_credentials() {
        let empty_id = FaucetSecretKeySource::AwsSecretsManager {
            secret_id: String::new(),
            version_id: "version-1".into(),
            region: "us-west-2".into(),
        };
        let empty_version = FaucetSecretKeySource::AwsSecretsManager {
            secret_id: "thruput/devnet/faucet".into(),
            version_id: String::new(),
            region: "us-west-2".into(),
        };
        let empty_region = FaucetSecretKeySource::AwsSecretsManager {
            secret_id: "thruput/devnet/faucet".into(),
            version_id: "version-1".into(),
            region: String::new(),
        };

        assert!(DefaultFaucetSecretKeyLoader.load(&empty_id).await.is_err());
        assert!(
            DefaultFaucetSecretKeyLoader
                .load(&empty_version)
                .await
                .is_err()
        );
        assert!(
            DefaultFaucetSecretKeyLoader
                .load(&empty_region)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn aws_request_pins_version_id() {
        let response = http::Response::builder()
            .status(200)
            .body(SdkBody::from(
                r#"{"VersionId":"version-1","SecretString":"abab"}"#,
            ))
            .expect("bug: fake response should build");
        let (http_client, captured_request) = capture_request(Some(response));
        let config = aws_sdk_secretsmanager::Config::builder()
            .behavior_version_latest()
            .credentials_provider(Credentials::for_tests())
            .region(Region::new("us-west-2"))
            .http_client(http_client)
            .build();
        let client = Client::from_conf(config);

        let _ = request_aws_secret(&client, "thruput/devnet/faucet", "version-1").await;

        let request = captured_request.expect_request();
        let body = std::str::from_utf8(
            request
                .body()
                .bytes()
                .expect("bug: captured request should have bytes"),
        )
        .expect("bug: AWS JSON request should be UTF-8");
        assert!(body.contains(r#""SecretId":"thruput/devnet/faucet""#));
        assert!(body.contains(r#""VersionId":"version-1""#));
    }

    #[test]
    fn aws_secret_string_is_parsed_without_exposing_payload() {
        let secret_value = format!("0x{}", "cd".repeat(32));
        let output = GetSecretValueOutput::builder()
            .secret_string(secret_value.clone())
            .version_id("version-1")
            .build();

        let key = parse_aws_secret_output(output, "thruput/devnet/faucet", "version-1")
            .expect("bug: valid AWS SecretString should parse");

        assert_eq!(key.as_ref(), &[0xCD; 32]);

        let uppercase_prefix = GetSecretValueOutput::builder()
            .secret_string(format!("0X{}", "cd".repeat(32)))
            .version_id("version-1")
            .build();
        let key = parse_aws_secret_output(uppercase_prefix, "thruput/devnet/faucet", "version-1")
            .expect("bug: uppercase-prefixed AWS SecretString should parse");
        assert_eq!(key.as_ref(), &[0xCD; 32]);

        let malformed = GetSecretValueOutput::builder()
            .secret_string(secret_value.clone() + "00")
            .version_id("version-1")
            .build();
        let error = parse_aws_secret_output(malformed, "thruput/devnet/faucet", "version-1")
            .expect_err("bug: malformed AWS SecretString should fail");
        assert!(!error.to_string().contains(&secret_value));
    }

    #[test]
    fn aws_secret_requires_secret_string() {
        let output = GetSecretValueOutput::builder()
            .secret_binary(Blob::new(vec![0xAB; 32]))
            .version_id("version-1")
            .build();

        let error = parse_aws_secret_output(output, "thruput/devnet/faucet", "version-1")
            .expect_err("bug: missing SecretString should fail");

        assert!(error.to_string().contains("SecretString"), "{error}");
    }

    #[test]
    fn aws_secret_requires_requested_version() {
        let output = GetSecretValueOutput::builder()
            .secret_string("ab".repeat(32))
            .version_id("version-2")
            .build();

        let error = parse_aws_secret_output(output, "thruput/devnet/faucet", "version-1")
            .expect_err("bug: mismatched secret version should fail");

        let message = error.to_string();
        assert!(message.contains("version-1"), "{message}");
        assert!(message.contains("version-2"), "{message}");
    }
}
