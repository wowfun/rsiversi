use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use rsi_meta_loader::{ApiVersion, ContentHash, ExpectedHashes, PluginLoader};

fn argument(arguments: &[OsString], index: usize, name: &str) -> io::Result<OsString> {
    arguments.get(index).cloned().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("missing {name} argument"),
        )
    })
}

fn utf8_argument(arguments: &[OsString], index: usize, name: &str) -> io::Result<String> {
    argument(arguments, index, name)?
        .into_string()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("{name} is not UTF-8")))
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() != 5 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected: MANIFEST CACHE MANIFEST_HASH ARTIFACT_HASH HOST_TARGET",
        )
        .into());
    }

    let manifest = PathBuf::from(argument(&arguments, 0, "manifest")?);
    let cache = PathBuf::from(argument(&arguments, 1, "cache")?);
    let manifest_hash = utf8_argument(&arguments, 2, "manifest hash")?.parse::<ContentHash>()?;
    let artifact_hash = utf8_argument(&arguments, 3, "artifact hash")?.parse::<ContentHash>()?;
    let host_target = utf8_argument(&arguments, 4, "host target")?;
    PluginLoader::new(cache, host_target, ApiVersion::CURRENT)
        .stage(manifest, ExpectedHashes::new(manifest_hash, artifact_hash))?;
    Ok(())
}
