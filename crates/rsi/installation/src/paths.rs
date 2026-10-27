use crate::{Result, error};
use rsi_host::HostPaths;
use std::path::{Path, PathBuf};

/// Resolves standard platform locations without creating directories or finding a Profile.
#[cfg(not(windows))]
pub fn standard_paths() -> Result<HostPaths> {
    let config = optional_environment_path("XDG_CONFIG_HOME")?;
    let state = optional_environment_path("XDG_STATE_HOME")?;
    let cache = optional_environment_path("XDG_CACHE_HOME")?;
    let home = (config.is_none() || state.is_none() || cache.is_none())
        .then(|| environment_path("HOME"))
        .transpose()?;
    standard_paths_from(config, state, cache, home.as_deref())
}

/// Resolves explicit XDG overrides or Windows Known Folders without creating directories.
#[cfg(windows)]
pub fn standard_paths() -> Result<HostPaths> {
    let config = optional_environment_path("XDG_CONFIG_HOME")?;
    let state = optional_environment_path("XDG_STATE_HOME")?;
    let cache = optional_environment_path("XDG_CACHE_HOME")?;
    let defaults = if config.is_none() || state.is_none() || cache.is_none() {
        Some(windows_standard_paths(
            dirs::config_dir(),
            dirs::data_local_dir(),
        )?)
    } else {
        None
    };
    let resolve = |override_path: Option<PathBuf>, fallback: Option<&Path>| {
        override_path
            .map(|path| path.join("rsi"))
            .or_else(|| fallback.map(Path::to_owned))
            .ok_or_else(|| error("Windows standard path is unavailable"))
    };
    HostPaths::new(
        resolve(config, defaults.as_ref().map(HostPaths::config))?,
        resolve(state, defaults.as_ref().map(HostPaths::state))?,
        resolve(cache, defaults.as_ref().map(HostPaths::cache))?,
    )
    .map_err(error)
}

#[cfg(windows)]
fn windows_standard_paths(roaming: Option<PathBuf>, local: Option<PathBuf>) -> Result<HostPaths> {
    let required = |path: Option<PathBuf>, folder: &str| {
        path.filter(|path| path.is_absolute()).ok_or_else(|| {
            error(format!(
                "required Windows Known Folder `{folder}` is unavailable"
            ))
        })
    };
    let config = required(roaming, "RoamingAppData")?.join("rsi");
    let local = required(local, "LocalAppData")?.join("rsi");
    HostPaths::new(config, local.join("state"), local.join("cache")).map_err(error)
}

#[cfg(not(windows))]
fn standard_paths_from(
    config: Option<PathBuf>,
    state: Option<PathBuf>,
    cache: Option<PathBuf>,
    home: Option<&Path>,
) -> Result<HostPaths> {
    let fallback = |suffix: &str| {
        home.map(|home| home.join(suffix))
            .ok_or_else(|| error("required environment path `HOME` is not configured"))
    };
    let config = config.map_or_else(|| fallback(".config"), Ok)?.join("rsi");
    let state = state
        .map_or_else(|| fallback(".local/state"), Ok)?
        .join("rsi");
    let cache = cache.map_or_else(|| fallback(".cache"), Ok)?.join("rsi");
    HostPaths::new(config, state, cache).map_err(error)
}

#[cfg(not(windows))]
fn environment_path(name: &str) -> Result<PathBuf> {
    optional_environment_path(name)?.ok_or_else(|| {
        error(format!(
            "required environment path `{name}` is not configured"
        ))
    })
}

fn optional_environment_path(name: &str) -> Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(error(format!("environment path `{name}` must be absolute")));
    }
    Ok(Some(path))
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
        Ok(optional_environment_path("HOME")?
            .ok_or_else(|| error("HOME unavailable"))?
            .join(".local"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(not(windows))]
    fn complete_xdg_paths_do_not_require_home() {
        let paths = standard_paths_from(
            Some(PathBuf::from("/config")),
            Some(PathBuf::from("/state")),
            Some(PathBuf::from("/cache")),
            None,
        )
        .unwrap();

        assert_eq!(paths.config(), Path::new("/config/rsi"));
        assert_eq!(paths.state(), Path::new("/state/rsi"));
        assert_eq!(paths.cache(), Path::new("/cache/rsi"));
    }

    #[test]
    #[cfg(not(windows))]
    fn missing_xdg_path_requires_home_for_its_fallback() {
        assert!(
            standard_paths_from(
                Some(PathBuf::from("/config")),
                None,
                Some(PathBuf::from("/cache")),
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("HOME")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_locations_separate_roaming_config_from_local_data() {
        let roaming = PathBuf::from(r"D:\fixture\漫游");
        let local = PathBuf::from(r"D:\fixture\Local😀");
        let paths = windows_standard_paths(Some(roaming.clone()), Some(local.clone())).unwrap();
        assert_eq!(paths.config(), roaming.join("rsi"));
        assert_eq!(paths.state(), local.join("rsi/state"));
        assert_eq!(paths.cache(), local.join("rsi/cache"));
        for (roaming, local, folder) in [
            (None, Some(local.clone()), "RoamingAppData"),
            (Some(roaming.clone()), None, "LocalAppData"),
            (
                Some(PathBuf::from("relative")),
                Some(local.clone()),
                "RoamingAppData",
            ),
            (Some(PathBuf::new()), Some(local.clone()), "RoamingAppData"),
            (
                Some(roaming.clone()),
                Some(PathBuf::from("relative")),
                "LocalAppData",
            ),
            (Some(roaming), Some(PathBuf::new()), "LocalAppData"),
        ] {
            assert!(
                windows_standard_paths(roaming, local)
                    .unwrap_err()
                    .to_string()
                    .contains(folder)
            );
        }
    }
}
