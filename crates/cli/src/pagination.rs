use thruput_json_rpc_types::events::PaginationCursor;

pub(crate) const DEFAULT_PAGE_LIMIT: u32 = 50;

#[derive(Debug)]
pub(crate) struct InvalidPaginationArgument {
    pub(crate) input: String,
    pub(crate) message: String,
}

pub(crate) fn parse_limit(input: Option<&str>) -> Result<u32, InvalidPaginationArgument> {
    let Some(input) = input else {
        return Ok(DEFAULT_PAGE_LIMIT);
    };
    let limit = input
        .parse::<u32>()
        .map_err(|error| InvalidPaginationArgument {
            input: input.to_owned(),
            message: error.to_string(),
        })?;
    if limit == 0 {
        return Err(InvalidPaginationArgument {
            input: input.to_owned(),
            message: "must be greater than zero".into(),
        });
    }
    Ok(limit)
}

pub(crate) fn parse_cursor(
    input: Option<&str>,
) -> Result<PaginationCursor, InvalidPaginationArgument> {
    let Some(input) = input else {
        return Ok(PaginationCursor::default());
    };
    serde_json::from_value(serde_json::Value::String(input.to_owned())).map_err(|error| {
        InvalidPaginationArgument {
            input: input.to_owned(),
            message: error.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_PAGE_LIMIT, parse_cursor, parse_limit};

    #[test]
    fn page_limit_defaults_and_accepts_nonzero_u32() {
        assert_eq!(parse_limit(None).unwrap(), DEFAULT_PAGE_LIMIT);
        assert_eq!(parse_limit(Some("1")).unwrap(), 1);
        assert_eq!(parse_limit(Some(&u32::MAX.to_string())).unwrap(), u32::MAX);
    }

    #[test]
    fn page_limit_rejects_invalid_values_and_preserves_input() {
        for input in ["0", "-1", "one", "4294967296"] {
            let error = parse_limit(Some(input)).expect_err("bug: invalid limit should fail");
            assert_eq!(error.input, input);
            assert!(!error.message.is_empty());
        }
    }

    #[test]
    fn pagination_cursor_uses_shared_hex_contract() {
        for (input, expected) in [
            (None, vec![]),
            (Some("0x"), vec![]),
            (Some("0XAbCd"), vec![0xab, 0xcd]),
        ] {
            let cursor = parse_cursor(input).expect("bug: shared cursor form should parse");
            assert_eq!(cursor.0, expected);
        }
    }

    #[test]
    fn pagination_cursor_rejects_malformed_hex_and_preserves_input() {
        for input in ["0xabc", "0xgg"] {
            let error = parse_cursor(Some(input)).expect_err("bug: invalid cursor should fail");
            assert_eq!(error.input, input);
            assert!(!error.message.is_empty());
        }
    }
}
