//! Request parsing helpers.

use bgh_core::prelude::*;
use serde::{Deserialize, Deserializer};

/// Distinguish an absent key (`None`) from an explicit `null`
/// (`Some(None)`); use with `#[serde(default, deserialize_with = "nullable")]`.
pub fn nullable<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Trimmed, non-empty text of at most `max` characters.
pub fn validate_text(
    resource: &str,
    field: &str,
    value: Option<&str>,
    max: usize,
) -> ApiResult<String> {
    let v = value.map(str::trim).unwrap_or("");
    if v.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            resource, field,
        )));
    }
    if v.chars().count() > max {
        return Err(ApiError::invalid_field(FieldError::custom(
            resource,
            field,
            format!("{field} is too long (maximum is {max} characters)"),
        )));
    }
    Ok(v.to_string())
}
