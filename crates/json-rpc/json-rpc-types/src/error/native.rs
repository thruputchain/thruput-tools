//! Native application errors. These types do not change the serving wire-13 catalogue.
//!
//! Cause, attempt admission and recovery are separate. Decoding an error never
//! grants permission to submit, replace a transaction, or repeat a funding call.

mod catalogue;
mod data;
mod reader;
mod schema;
pub use catalogue::{ERROR_INFOS, METHODS, codes, error_info_by_code, method_error_codes};
pub use data::*;
pub use reader::DecodedError;
pub use schema::{data_schema, method_data_schema};
