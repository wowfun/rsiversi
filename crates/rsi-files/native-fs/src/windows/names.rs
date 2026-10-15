//! Preflight native path spelling before acquiring any authority.
use std::{io, os::windows::ffi::OsStrExt as _, path::Path};

pub(super) const MAX_COMPONENT_UNITS: usize = 255;

pub(super) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn separator(unit: u16) -> bool {
    unit == u16::from(b'/') || unit == u16::from(b'\\')
}

pub(super) fn absolute(path: &Path) -> io::Result<(u16, Vec<Vec<u16>>)> {
    let units: Vec<_> = path.as_os_str().encode_wide().take(32768).collect();
    if units.len() > 32767
        || units.len() < 3
        || !(u16::from(b'A')..=u16::from(b'Z')).contains(&units[0])
        || units[1] != u16::from(b':')
        || !separator(units[2])
    {
        return Err(invalid("uppercase local drive root required"));
    }
    Ok((units[0], components(&units[3..])?))
}

pub(super) fn relative(path: &Path) -> io::Result<Vec<Vec<u16>>> {
    let units: Vec<_> = path.as_os_str().encode_wide().take(32768).collect();
    if units.len() > 32767 {
        return Err(invalid("native path exceeds its bound"));
    }
    components(&units)
}

fn components(units: &[u16]) -> io::Result<Vec<Vec<u16>>> {
    if units.is_empty() {
        return Ok(Vec::new());
    }
    units
        .split(|unit| separator(*unit))
        .map(|component| {
            validate_component(component)?;
            Ok(component.to_vec())
        })
        .collect()
}

pub(super) fn validate_component(name: &[u16]) -> io::Result<()> {
    if name.is_empty()
        || name.len() > MAX_COMPONENT_UNITS
        || matches!(name.last(), Some(32 | 46))
        || name
            .iter()
            .any(|unit| *unit < 32 || [34, 42, 47, 58, 60, 62, 63, 92, 124].contains(unit))
    {
        return Err(invalid("invalid Windows path component"));
    }
    let mut stem: Vec<_> = name
        .iter()
        .copied()
        .take_while(|unit| *unit != 46)
        .map(|unit| {
            if (97..=122).contains(&unit) {
                unit - 32
            } else {
                unit
            }
        })
        .collect();
    while stem.last() == Some(&32) {
        stem.pop();
    }
    let reserved = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|name| stem.iter().copied().eq(name.encode_utf16()));
    let serial = stem.len() == 4
        && (stem[..3] == [67, 79, 77] || stem[..3] == [76, 80, 84])
        && ((49..=57).contains(&stem[3]) || [178, 179, 185].contains(&stem[3]));
    if reserved || serial {
        return Err(invalid("DOS device names are not file components"));
    }
    Ok(())
}
