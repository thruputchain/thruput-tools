//! Native application errors.
//!
//! Cause, attempt admission and recovery are separate. Decoding an error never
//! grants permission to submit, replace a transaction, or repeat a funding call.

mod catalogue;
mod data;
mod reader;
mod schema;
pub use catalogue::{
    ERROR_INFOS, METHODS, codes, error_info_by_code, method_error_codes, outcome_for_code,
};
pub use data::*;
pub use reader::DecodedError;
pub use schema::{data_schema, method_data_schema};
