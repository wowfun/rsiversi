use crate::{Result, error};
use rsi_host::HostPaths;
use std::path::{Path, PathBuf};

/// Explicit final roots; absent entries retain ordinary platform defaults.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HomeOverrides {
    /// User configuration home.
    pub config: Option<PathBuf>,
    /// Credentials, packages and media home.
    pub data: Option<PathBuf>,
    /// Durable product state home.
    pub state: Option<PathBuf>,
    /// Rebuildable cache home.
    pub cache: Option<PathBuf>,
    /// Private operation scratch parent.
    pub tmp: Option<PathBuf>,
}

impl HomeOverrides {
    /// Freeze selected roots without creating directories. Defaults are consulted only for absent overrides.
    pub fn resolve(&self, cwd: &Path) -> Result<HostPaths> {
        self.resolve_with(cwd, |index| {
            if index == 4 {
                return Ok(std::env::temp_dir());
            }
            let (name, suffix) = [
                ("XDG_CONFIG_HOME", ".config"),
                ("XDG_DATA_HOME", ".local/share"),
                ("XDG_STATE_HOME", ".local/state"),
                ("XDG_CACHE_HOME", ".cache"),
            ][index];
            let value = std::env::var_os(name).map(PathBuf::from);
            let home = value
                .as_ref()
                .is_none_or(|p| !p.is_absolute())
                .then(dirs::home_dir)
                .flatten();
            let (root, ignored) = resolve_xdg(value.as_deref(), home.as_deref(), suffix)?;
            if ignored {
                eprintln!(
                    "Ignoring relative {name}; default root is {}",
                    root.display()
                );
            }
            Ok(root)
        })
    }
    fn resolve_with(
        &self,
        cwd: &Path,
        mut fallback: impl FnMut(usize) -> Result<PathBuf>,
    ) -> Result<HostPaths> {
        if !cwd.is_absolute() {
            return Err(error("invoking directory must be absolute"));
        }
        let selected = [
            &self.config,
            &self.data,
            &self.state,
            &self.cache,
            &self.tmp,
        ];
        let [config, data, state, cache, tmp] =
            std::array::from_fn(|index| match selected[index] {
                Some(path) => normalize_path(path, cwd),
                None => fallback(index).and_then(|path| {
                    if !path.is_absolute() {
                        return Err(error("default home path must be absolute"));
                    }
                    normalize_path(&path, cwd)
                }),
            });
        let paths = HostPaths::new(config?, data?, state?, cache?, tmp?).map_err(error)?;
        crate::receipts::validate_home_layout(&paths)?;
        Ok(paths)
    }
}
/// Resolve a nonempty path lexically against an absolute invoking directory, without filesystem I/O.
pub fn normalize_path(path: &Path, cwd: &Path) -> Result<PathBuf> {
    if !cwd.is_absolute() {
        return Err(error("invoking directory must be absolute"));
    }
    if path.as_os_str().is_empty() {
        return Err(error("home path is empty"));
    }
    let joined = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    let mut normalized = PathBuf::new();
    for part in joined.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(error("path escapes its filesystem root"));
                }
            }
            part => normalized.push(part.as_os_str()),
        }
    }
    if !normalized.is_absolute() {
        return Err(error("path is not absolute"));
    }
    Ok(normalized)
}
/// Resolves home-based XDG roots and an OS scratch parent without creating them.
pub fn standard_paths() -> Result<HostPaths> {
    HomeOverrides::default().resolve(&std::env::current_dir()?)
}

fn resolve_xdg(value: Option<&Path>, home: Option<&Path>, suffix: &str) -> Result<(PathBuf, bool)> {
    if let Some(value) = value.filter(|value| value.is_absolute()) {
        return Ok((value.join("rsiversi"), false));
    }
    let ignored = value.is_some_and(|value| !value.as_os_str().is_empty());
    let home = home
        .filter(|path| path.is_absolute())
        .ok_or_else(|| error("user home is unavailable for XDG defaults"))?;
    Ok((home.join(suffix).join("rsiversi"), ignored))
}

/// Bounded legacy credential discovery; never reads contents, imports or deletes.
pub fn legacy_credentials() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(value) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        candidates.push(value.join("rsi/credentials/credentials.json"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".config/rsi/credentials/credentials.json"));
    }
    #[cfg(windows)]
    if let Some(roaming) = dirs::config_dir() {
        candidates.push(roaming.join("rsi/credentials/credentials.json"));
    }
    candidates.sort();
    candidates.dedup();
    candidates.retain(|path| std::fs::symlink_metadata(path).is_ok());
    candidates
}

pub fn default_prefix() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        Ok(dirs::data_local_dir()
            .ok_or_else(|| error("LocalAppData unavailable"))?
            .join("RSIversi"))
    }
    #[cfg(not(windows))]
    {
        Ok(dirs::home_dir()
            .ok_or_else(|| error("user home unavailable"))?
            .join(".local"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_scratch_does_not_consult_an_invalid_unused_default() {
        let root = tempfile::tempdir().unwrap();
        for unused in ["", "relative"] {
            let selected = HomeOverrides {
                tmp: Some("scratch".into()),
                ..HomeOverrides::default()
            };
            let paths = selected
                .resolve_with(root.path(), |index| {
                    assert_ne!(index, 4, "explicit scratch must skip OS default {unused:?}");
                    Ok(root
                        .path()
                        .join(["config", "data", "state", "cache"][index]))
                })
                .unwrap();
            assert_eq!(paths.tmp(), root.path().join("scratch"));
            assert!(
                HomeOverrides::default()
                    .resolve_with(root.path(), |index| {
                        Ok(if index == 4 {
                            PathBuf::from(unused)
                        } else {
                            root.path()
                                .join(["config", "data", "state", "cache"][index])
                        })
                    })
                    .is_err()
            );
        }
    }

    #[test]
    fn default_roots_normalize_trailing_separators_without_creating_them() {
        let root = tempfile::tempdir().unwrap();
        let paths = HomeOverrides::default()
            .resolve_with(root.path(), |index| {
                let mut path = root
                    .path()
                    .join(["config", "data", "state", "cache", "scratch"][index]);
                path.push("");
                Ok(path)
            })
            .unwrap();
        assert_eq!(
            paths.tmp().as_os_str(),
            root.path().join("scratch").as_os_str()
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn home_layout_allows_independent_nesting_and_refuses_managed_target_overlap() {
        let root = tempfile::tempdir().unwrap();
        let make = |config: &Path, data: &Path, state: &Path, cache: &Path| {
            HostPaths::new(config, data, state, cache, root.path().join("tmp")).unwrap()
        };
        let config = root.path().join("config");
        let data = config.join("independent/data");
        let state = root.path().join("state");
        let cache = root.path().join("cache");
        for paths in [
            make(&config, &data, &state, &cache),
            make(&config, &config, &config, &cache),
        ] {
            crate::receipts::validate_home_layout(&paths).unwrap();
        }
        for paths in [
            make(&config, &config, &config, &config),
            HostPaths::new(&config, &data, &state, &cache, data.join("media")).unwrap(),
            make(&config, &state.join("agent/data"), &state, &cache),
            make(&config, &data, &config.join("skills/state"), &cache),
            make(&config, &data, &state, &data.join("media/cache")),
            make(&config, &cache.join("history/data"), &state, &cache),
        ] {
            assert!(crate::receipts::validate_home_layout(&paths).is_err());
        }
        assert!(!config.exists());
        assert!(!state.exists());
        assert!(!cache.exists());
    }

    #[cfg(windows)]
    #[test]
    fn home_layout_refuses_case_variants_of_managed_entries() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        for name in ["AGENTS.md", "agents.md", "aGeNtS.Md"] {
            let paths = HostPaths::new(
                &config,
                config.join(name).join("data"),
                root.path().join("state"),
                root.path().join("cache"),
                root.path().join("tmp"),
            )
            .unwrap();
            assert!(
                crate::receipts::validate_home_layout(&paths).is_err(),
                "{name}"
            );
        }
        assert!(!config.exists());
    }

    #[test]
    fn xdg_resolution_uses_the_same_namespace_on_every_platform() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let selected = root.path().join("selected");
        for suffix in [".config", ".local/share", ".local/state", ".cache"] {
            assert_eq!(
                resolve_xdg(Some(&selected), None, suffix).unwrap(),
                (selected.join("rsiversi"), false)
            );
            for value in [None, Some(Path::new("")), Some(Path::new("relative"))] {
                let (path, ignored) = resolve_xdg(value, Some(&home), suffix).unwrap();
                assert_eq!(path, home.join(suffix).join("rsiversi"));
                assert_eq!(ignored, value == Some(Path::new("relative")));
            }
            assert!(resolve_xdg(None, None, suffix).is_err());
        }
        assert!(!home.exists());
        assert!(!selected.exists());
    }
}
