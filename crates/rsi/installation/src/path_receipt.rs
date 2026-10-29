//! Exact PATH deltas; registry representation and user text remain intact.
#[cfg(unix)]
use crate::filesystem as fs;
use crate::{Result, error};
use serde::{Deserialize, Serialize};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathEdit {
    pub target: String,
    pub insertion: String,
    pub applied: bool,
}
impl PathEdit {
    pub fn validate(&self, id: &str, bin: &Path) -> Result<()> {
        #[cfg(windows)]
        let valid = self.target == "user-path" && self.insertion == bin.to_string_lossy();
        #[cfg(unix)]
        let valid = [".profile", ".bashrc", ".zshrc"].contains(&self.target.as_str())
            && self.insertion == block(id, bin);
        #[cfg(windows)]
        let _ = id;
        if !valid {
            return Err(error("invalid PATH receipt"));
        }
        Ok(())
    }
}
#[cfg(unix)]
fn block(id: &str, bin: &Path) -> String {
    format!(
        "\n# rsi-install:{id}\nexport PATH='{}':\"$PATH\"\n# rsi-install:{id}:end\n",
        bin.to_string_lossy().replace('\'', "'\\''")
    )
}
#[cfg(unix)]
fn home() -> Result<PathBuf> {
    let path = PathBuf::from(std::env::var_os("HOME").ok_or_else(|| error("HOME unavailable"))?);
    if !path.is_absolute() {
        return Err(error("HOME must be absolute"));
    }
    Ok(path)
}
pub fn planned(id: &str, bin: &Path) -> Result<Vec<PathEdit>> {
    #[cfg(windows)]
    {
        let _ = id;
        let (_, raw) = registry()?;
        if raw
            .split(';')
            .any(|entry| entry.eq_ignore_ascii_case(&bin.to_string_lossy()))
        {
            return Ok(vec![]);
        }
        Ok(vec![PathEdit {
            target: "user-path".into(),
            insertion: bin.to_string_lossy().into_owned(),
            applied: false,
        }])
    }
    #[cfg(unix)]
    {
        let home = home()?;
        Ok([".profile", ".bashrc", ".zshrc"]
            .into_iter()
            .filter(|name| *name != ".zshrc" || home.join(name).exists())
            .map(|name| PathEdit {
                target: name.into(),
                insertion: block(id, bin),
                applied: false,
            })
            .collect())
    }
}
pub fn apply(edit: &PathEdit, remove: bool) -> Result<()> {
    #[cfg(windows)]
    {
        use winreg::{
            RegKey,
            enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE},
        };
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags("Environment", KEY_READ | KEY_SET_VALUE)?;
        apply_registry(&key, edit, remove, crate::windows_environment::notify)?;
    }
    #[cfg(unix)]
    {
        use rsi_files_native_fs as native;
        let home = home()?;
        let root = native::open_absolute_directory_no_follow(&home)?;
        if let Ok(meta) = std::fs::symlink_metadata(home.join(&edit.target))
            && (!meta.is_file() || meta.file_type().is_symlink() || meta.permissions().readonly())
        {
            return Err(error(format!(
                "{} is linked or read-only; configure PATH manually",
                edit.target
            )));
        }
        let bytes = fs::read(&root, &edit.target, 1024 * 1024)?.unwrap_or_default();
        let text = String::from_utf8(bytes).map_err(error)?;
        let changed = if remove {
            text.replace(&edit.insertion, "")
        } else if text.contains(&edit.insertion) {
            text.clone()
        } else {
            format!("{text}{}", edit.insertion)
        };
        if text != changed {
            fs::write(&root, &edit.target, changed.as_bytes(), true)?;
        } else if remove {
            eprintln!(
                "PATH block was not removed: {} no longer contains the exact installation marker; inspect it manually",
                edit.target
            );
        }
    }
    Ok(())
}
#[cfg(windows)]
fn apply_registry(
    key: &winreg::RegKey,
    edit: &PathEdit,
    remove: bool,
    notify: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let (kind, raw) = registry_value(key)?;
    let text = edit_registry_text(&raw, &edit.insertion, remove);
    if text == raw {
        if remove {
            eprintln!(
                "PATH entry was not removed: its current value no longer uniquely matches the installation receipt: {}",
                edit.insertion
            );
        }
        return Ok(());
    }
    let bytes = text
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    key.set_raw_value("Path", &winreg::RegValue { bytes, vtype: kind })?;
    if let Err(error) = notify() {
        eprintln!(
            "PATH saved, but shell refresh failed: {error}; restart the terminal application or sign out and back in before using the new PATH"
        );
    }
    Ok(())
}
#[cfg(windows)]
fn registry() -> Result<(winreg::enums::RegType, String)> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Environment")?;
    registry_value(&key)
}
#[cfg(windows)]
fn registry_value(key: &winreg::RegKey) -> Result<(winreg::enums::RegType, String)> {
    use winreg::enums::REG_EXPAND_SZ;
    let value = match key.get_raw_value("Path") {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((REG_EXPAND_SZ, String::new()));
        }
        Err(e) => return Err(e),
    };
    decode_registry_value(value)
}
#[cfg(windows)]
fn decode_registry_value(value: winreg::RegValue) -> Result<(winreg::enums::RegType, String)> {
    use winreg::enums::{REG_EXPAND_SZ, REG_SZ};
    if !matches!(value.vtype, REG_SZ | REG_EXPAND_SZ)
        || value.bytes.len() > 65536
        || !value.bytes.len().is_multiple_of(2)
    {
        return Err(error("unsupported user PATH registry value"));
    }
    let mut words: Vec<_> = value
        .bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    if words.last() == Some(&0) {
        words.pop();
    }
    Ok((value.vtype, String::from_utf16(&words).map_err(error)?))
}
#[cfg(windows)]
fn edit_registry_text(raw: &str, insertion: &str, remove: bool) -> String {
    let mut entries: Vec<&str> = if raw.is_empty() {
        vec![]
    } else {
        raw.split(';').collect()
    };
    if remove {
        if entries.iter().filter(|part| **part == insertion).count() != 1 {
            return raw.into();
        }
        entries.retain(|part| *part != insertion);
    } else if !entries
        .iter()
        .any(|part| part.eq_ignore_ascii_case(insertion))
    {
        entries.push(insertion);
    }
    entries.join(";")
}
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn registry_deltas_preserve_variables_type_and_unrelated_text() {
        for kind in [winreg::enums::REG_SZ, winreg::enums::REG_EXPAND_SZ] {
            let raw = r"%USERPROFILE%\bin;;C:\手写 Path;";
            let value = winreg::RegValue {
                vtype: kind.clone(),
                bytes: raw
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect(),
            };
            let (retained_kind, text) = decode_registry_value(value).unwrap();
            assert_eq!(retained_kind, kind);
            assert_eq!(text, raw);
            let inserted = edit_registry_text(&text, r"C:\rsi\bin", false);
            assert_eq!(edit_registry_text(&inserted, r"C:\rsi\bin", true), raw);
        }
        assert_eq!(edit_registry_text("", r"C:\rsi\bin", false), r"C:\rsi\bin");
        let user_changed = r"C:\rsi\bin;C:\rsi\bin";
        assert_eq!(
            edit_registry_text(user_changed, r"C:\rsi\bin", true),
            user_changed
        );
    }
    #[test]
    fn registry_apply_preserves_type_and_notifies_after_each_actual_write() {
        use winreg::{
            RegKey,
            enums::{HKEY_CURRENT_USER, REG_EXPAND_SZ, REG_SZ},
        };
        let parent = RegKey::predef(HKEY_CURRENT_USER);
        let name = format!(
            "Software\\rsi-path-test-{}",
            crate::filesystem::id().unwrap()
        );
        let (key, _) = parent.create_subkey(&name).unwrap();
        let edit = PathEdit {
            target: "user-path".into(),
            insertion: r"C:\rsi-test\bin".into(),
            applied: true,
        };
        let raw = r"%USERPROFILE%\bin;;C:\User Content;";
        for kind in [REG_SZ, REG_EXPAND_SZ] {
            key.set_raw_value(
                "Path",
                &winreg::RegValue {
                    vtype: kind.clone(),
                    bytes: raw
                        .encode_utf16()
                        .chain([0])
                        .flat_map(u16::to_le_bytes)
                        .collect(),
                },
            )
            .unwrap();
            let notifications = std::cell::Cell::new(0);
            apply_registry(&key, &edit, false, || {
                assert_eq!(
                    registry_value(&key).unwrap(),
                    (kind.clone(), format!("{raw};{}", edit.insertion))
                );
                notifications.set(notifications.get() + 1);
                Ok(())
            })
            .unwrap();
            apply_registry(&key, &edit, true, || {
                assert_eq!(registry_value(&key).unwrap(), (kind.clone(), raw.into()));
                notifications.set(notifications.get() + 1);
                Ok(())
            })
            .unwrap();
            apply_registry(&key, &edit, true, || {
                panic!("no notification for unchanged PATH")
            })
            .unwrap();
            assert_eq!(notifications.get(), 2);
        }
        drop(key);
        parent.delete_subkey(&name).unwrap();
    }
}
