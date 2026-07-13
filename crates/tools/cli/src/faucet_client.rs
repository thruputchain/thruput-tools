use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::CliError;

const FAUCET_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

pub(crate) fn client() -> Result<reqwest::blocking::Client, CliError> {
    reqwest::blocking::Client::builder()
        .timeout(FAUCET_REQUEST_TIMEOUT)
        .build()
        .map_err(|error| CliError::Other(format!("failed to build faucet HTTP client: {error}")))
}

pub(crate) fn post_json<T: for<'de> Deserialize<'de>, B: Serialize>(
    client: &reqwest::blocking::Client,
    url: &str,
    body: &B,
) -> Result<T, CliError> {
    let response = client
        .post(url)
        .json(body)
        .send()
        .map_err(|error| CliError::Other(format!("failed to call `{url}`: {error}")))?;
    parse_faucet_response(response, url)
}

pub(crate) fn get_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<T, CliError> {
    let response = client
        .get(url)
        .send()
        .map_err(|error| CliError::Other(format!("failed to call `{url}`: {error}")))?;
    parse_faucet_response(response, url)
}

fn parse_faucet_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::blocking::Response,
    url: &str,
) -> Result<T, CliError> {
    let status = response.status();
    let bytes = response
        .bytes()
        .map_err(|error| CliError::Other(format!("failed to read `{url}` response: {error}")))?;
    if status.is_success() {
        return serde_json::from_slice(&bytes)
            .map_err(|error| CliError::Other(format!("invalid JSON from `{url}`: {error}")));
    }
    if let Ok(body) = serde_json::from_slice::<FaucetErrorBody>(&bytes) {
        let retry_after = body
            .error
            .retry_after_seconds
            .map(|seconds| format!("; retry after {seconds}s"))
            .unwrap_or_default();
        return Err(CliError::Other(format!(
            "faucet request `{url}` failed with {}: {} ({}){}",
            status, body.error.message, body.error.code, retry_after
        )));
    }
    Err(CliError::Other(format!(
        "faucet request `{url}` failed with {status}"
    )))
}

#[derive(Debug, Clone, Deserialize)]
struct FaucetErrorBody {
    error: FaucetErrorDetail,
}

#[derive(Debug, Clone, Deserialize)]
struct FaucetErrorDetail {
    code: String,
    message: String,
    #[serde(rename = "retryAfterSeconds")]
    retry_after_seconds: Option<u64>,
}
