//! Pure native argument readers shared by launchers and applications.

use crate::RsiError;
use std::{ffi::OsString, path::PathBuf};
type Result<T> = std::result::Result<T, RsiError>;

#[allow(missing_docs)] // Mechanical consumption; application owners define the grammar.
pub fn set_flag(value: &mut bool, name: &str) -> Result<()> {
    if *value {
        return Err(invalid(format!("duplicate {name}")));
    }
    *value = true;
    Ok(())
}

#[allow(missing_docs)] // Mechanical consumption; application owners define the grammar.
pub fn set_option<T>(slot: &mut Option<T>, value: T, name: &str) -> Result<()> {
    if slot.is_some() {
        return Err(invalid(format!("duplicate {name}")));
    }
    *slot = Some(value);
    Ok(())
}

#[allow(missing_docs)] // Mechanical consumption; application owners define the grammar.
pub fn path_value(arguments: &mut impl Iterator<Item = OsString>, option: &str) -> Result<PathBuf> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| invalid(format!("{option} requires a value")))
}

#[allow(missing_docs)] // Mechanical consumption; application owners define the grammar.
pub fn string_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<String> {
    let value = arguments
        .next()
        .ok_or_else(|| invalid(format!("{option} requires a value")))?;
    utf8(value)
}

#[allow(missing_docs)] // Mechanical consumption; application owners define the grammar.
pub fn utf8(value: OsString) -> Result<String> {
    value
        .into_string()
        .map_err(|_| invalid("CLI arguments must be UTF-8"))
}

fn invalid(message: impl Into<String>) -> RsiError {
    RsiError::Boot(message.into())
}
