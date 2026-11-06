use super::{
    ErrorData,
    catalogue::{EVIDENCE_FIELDS, allowed_evidence, kind_allowed, kind_code},
};
use serde_json::Value;

/// Interpretation of a JSON-RPC **error object**, not the enclosing response.
/// Raw JSON survives every classification, including malformed/framework data.
/// Neither case authorizes automatic writes or transaction replacement.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DecodedError {
    /// Known code/family with valid method-specific evidence. Future reasons
    /// remain in the typed value and original JSON. Provenance is not verified.
    Known { data: Box<ErrorData>, raw: Value },
    /// Framework/unfamiliar/malformed data, or contradictory code/evidence.
    /// Preserve prior uncertainty and surface the original error.
    Unclassified { raw: Value },
}

impl DecodedError {
    /// Classify an error for the invoked native method. This does not compare
    /// hashes with the request, compute signed-envelope hashes, verify chain
    /// identity, or implement recovery policy. Such checks belong to callers.
    pub fn decode(method: &str, raw: Value) -> Self {
        let data = Self::classify(method, &raw);
        match data {
            Some(data) => Self::Known {
                data: Box::new(data),
                raw,
            },
            None => Self::Unclassified { raw },
        }
    }

    /// Complete original error, including unfamiliar fields and evidence.
    pub fn raw(&self) -> &Value {
        match self {
            Self::Known { raw, .. } | Self::Unclassified { raw } => raw,
        }
    }

    fn classify(method: &str, raw: &Value) -> Option<ErrorData> {
        let envelope = raw.as_object()?;
        let code = i32::try_from(envelope.get("code")?.as_i64()?).ok()?;
        envelope.get("message")?.as_str()?;
        let data = envelope.get("data")?;
        let kind = data.as_object()?.get("kind")?.as_str()?;
        if kind_code(kind)? != code
            || !kind_allowed(method, kind)
            || !valid_evidence(method, kind, data)
        {
            return None;
        }
        serde_json::from_value(data.clone()).ok()
    }
}

// Validate relational constraints before typed decoding. Field domains and
// known-variant requirements are validated by serde, even with future reasons.
pub(super) fn valid_evidence(method: &str, kind: &str, data: &Value) -> bool {
    for field in EVIDENCE_FIELDS {
        if data.get(field).is_some() && !allowed_evidence(method, kind, field) {
            return false;
        }
    }
    if method == "sendTransaction"
        && allowed_evidence(method, kind, "admission")
        && !matches!(
            data.get("admission").and_then(Value::as_str),
            Some("notAttempted" | "rejected" | "unknown")
        )
    {
        return false;
    }
    if kind == "transactionInvalid" && data["admission"] != "notAttempted" {
        return false;
    }
    if kind == "transactionRejected"
        && !matches!(
            data["admission"].as_str(),
            Some("notAttempted" | "rejected")
        )
    {
        return false;
    }
    if kind == "fundingAttemptStopped" {
        if (data.get("signedTransaction").is_some() || data.get("receipt").is_some())
            && data.get("transactionHash").is_none()
        {
            return false;
        }
        if let Some(receipt) = data.get("receipt")
            && receipt.get("transactionHash") != data.get("transactionHash")
        {
            return false;
        }
    }
    if kind == "localFundingUnresolved"
        && let Some(cause) = data.get("cause")
    {
        if cause["kind"] != "fundingAttemptStopped"
            || !valid_evidence(method, "fundingAttemptStopped", cause)
        {
            return false;
        }
        for field in ["transactionHash", "signedTransaction"] {
            if cause.get(field).is_some() && cause.get(field) != data.get(field) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rate_limited_is_typed_with_optional_decimal_delay_and_matching_code() {
        for delay in [None, Some(123), Some(u64::MAX)] {
            let data = serde_json::to_value(ErrorData::RateLimited {
                retry_after_ms: delay,
            })
            .unwrap();
            let mut expected = json!({"kind": "rateLimited"});
            if let Some(delay) = delay {
                expected["retryAfterMs"] = delay.to_string().into();
            }
            assert_eq!(data, expected);
            for method in ["getBalance", "sendTransaction"] {
                let raw = json!({"code": super::super::codes::RATE_LIMITED, "message": "Rate limited", "data": data});
                let decoded = DecodedError::decode(method, raw.clone());
                assert_eq!(decoded.raw(), &raw);
                assert!(
                    matches!(decoded, DecodedError::Known { data, .. } if matches!(*data, ErrorData::RateLimited { retry_after_ms } if retry_after_ms == delay))
                );
            }
        }
        for (method, code, data) in [
            ("getBalance", -32053, json!({"kind":"rateLimited"})),
            ("getBalance", -32055, json!({"kind":"overloaded"})),
            ("getVersionInfo", -32055, json!({"kind":"rateLimited"})),
            (
                "getBalance",
                -32055,
                json!({"kind":"rateLimited", "retryAfterMs":123}),
            ),
            (
                "getBalance",
                -32055,
                json!({"kind":"rateLimited", "retryAfterMs":null}),
            ),
            (
                "sendTransaction",
                -32055,
                json!({"kind":"rateLimited", "admission":"notAttempted"}),
            ),
        ] {
            assert!(matches!(
                DecodedError::decode(
                    method,
                    json!({"code":code, "message":"refused", "data":data})
                ),
                DecodedError::Unclassified { .. }
            ));
        }
    }
}
