use super::{
    MAXIMUM_NATIVE_ADDON_MANIFEST_BYTES, MAXIMUM_NATIVE_ADDON_SERVICES, MAXIMUM_NATIVE_ADDONS,
    NativeAddonError, NativeAddonRecord, Result,
};
use serde::{
    Deserialize, Deserializer,
    de::{Error as _, MapAccess, Visitor},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::File,
    io::Read as _,
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecordWire {
    id: String,
    plugin: String,
    target: String,
    artifact_sha256: String,
    portable_services: Vec<String>,
}
impl TryFrom<RecordWire> for NativeAddonRecord {
    type Error = NativeAddonError;
    fn try_from(wire: RecordWire) -> Result<Self> {
        identifier(&wire.id, 64)?;
        identifier(&wire.plugin, 256)?;
        identifier(&wire.target, 64)?;
        if !digest(&wire.artifact_sha256) {
            return Err(NativeAddonError::Invalid("artifact digest"));
        }
        Ok(Self {
            id: wire.id,
            plugin: wire.plugin,
            target: wire.target,
            artifact_sha256: wire.artifact_sha256,
            portable_services: services(wire.portable_services)?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    format: u32,
    pub(super) id: String,
    plugin: String,
    target: String,
    artifact: PathBuf,
    #[serde(default)]
    portable_services: Vec<String>,
    build: Option<Build>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Build {
    command: Vec<String>,
    #[serde(default)]
    watch: Vec<PathBuf>,
    #[serde(default = "build_timeout")]
    timeout_seconds: u64,
}
const fn build_timeout() -> u64 {
    120
}
impl Manifest {
    fn validate(mut self) -> Result<Self> {
        if self.format != 1 {
            return Err(NativeAddonError::Invalid("manifest format"));
        }
        identifier(&self.id, 64)?;
        identifier(&self.plugin, 256)?;
        identifier(&self.target, 64)?;
        relative(&self.artifact)?;
        self.portable_services = services(self.portable_services)?;
        if let Some(build) = &self.build {
            if build.command.is_empty()
                || build.command.len() > 64
                || build.command[0].is_empty()
                || build
                    .command
                    .iter()
                    .any(|argument| argument.len() > 4096 || argument.contains('\0'))
                || build.command.iter().map(String::len).sum::<usize>() > 16 * 1024
                || build.watch.len() > 256
                || !(1..=600).contains(&build.timeout_seconds)
            {
                return Err(NativeAddonError::Invalid("build bounds"));
            }
            for path in &build.watch {
                relative(path)?;
            }
        }
        Ok(self)
    }
    pub(super) fn into_record(self, artifact_sha256: String) -> NativeAddonRecord {
        NativeAddonRecord {
            id: self.id,
            plugin: self.plugin,
            target: self.target,
            artifact_sha256,
            portable_services: self.portable_services,
        }
    }
}

pub(super) fn read(path: &Path) -> Result<(Manifest, File)> {
    if path.as_os_str().len() > 4096 || path.components().count() > 64 {
        return Err(NativeAddonError::Invalid("manifest path bounds"));
    }
    let parent = path
        .parent()
        .ok_or(NativeAddonError::Invalid("manifest path"))?;
    let name = path
        .file_name()
        .ok_or(NativeAddonError::Invalid("manifest path"))?;
    let directory = rsi_files_native_fs::open_absolute_directory_no_follow(parent)?;
    let file = rsi_files_native_fs::open_relative_file_no_follow(&directory, Path::new(name))?;
    let bytes = bounded_read(file, MAXIMUM_NATIVE_ADDON_MANIFEST_BYTES)?;
    let text =
        std::str::from_utf8(&bytes).map_err(|_| NativeAddonError::Invalid("manifest UTF-8"))?;
    let manifest = toml::from_str::<Manifest>(text)
        .map_err(|_| NativeAddonError::Invalid("manifest syntax"))?
        .validate()?;
    let artifact =
        rsi_files_native_fs::open_relative_file_no_follow(&directory, &manifest.artifact)?;
    if !artifact.metadata()?.is_file() {
        return Err(NativeAddonError::Invalid("regular artifact required"));
    }
    Ok((manifest, artifact))
}

pub(super) fn bounded_read(file: File, maximum: usize) -> Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(NativeAddonError::Invalid("regular file required"));
    }
    if metadata.len() > maximum as u64 {
        return Err(NativeAddonError::Capacity("encoded bytes"));
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(NativeAddonError::Capacity("encoded bytes"));
    }
    Ok(bytes)
}
pub(super) fn identifier(value: &str, maximum: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > maximum
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(NativeAddonError::Invalid("identifier"));
    }
    Ok(())
}
pub(super) fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}
fn relative(path: &Path) -> Result<()> {
    let Some(text) = path.to_str() else {
        return Err(NativeAddonError::Invalid("relative path encoding"));
    };
    if text.is_empty()
        || text.len() > 4096
        || text.contains('\0')
        || path.components().count() > 32
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(NativeAddonError::Invalid(
            "normalized relative path required",
        ));
    }
    Ok(())
}
fn services(keys: Vec<String>) -> Result<Vec<String>> {
    if keys.len() > MAXIMUM_NATIVE_ADDON_SERVICES {
        return Err(NativeAddonError::Capacity("Portable keys"));
    }
    let mut unique = BTreeSet::new();
    let mut validation = rsi_agent_composition::AgentContributionCatalog::new([])
        .map_err(|_| NativeAddonError::Invalid("Portable keys"))?;
    for key in keys {
        validation
            .isolate_portable(key.clone())
            .map_err(|_| NativeAddonError::Invalid("Portable key"))?;
        if key.chars().any(char::is_control) || !unique.insert(key) {
            return Err(NativeAddonError::Invalid(
                "duplicate or control Portable key",
            ));
        }
    }
    Ok(unique.into_iter().collect())
}

pub(super) fn records<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, NativeAddonRecord>, D::Error> {
    struct Records;
    impl<'de> Visitor<'de> for Records {
        type Value = BTreeMap<String, NativeAddonRecord>;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("bounded unique native addon records")
        }
        fn visit_map<M: MapAccess<'de>>(
            self,
            mut map: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut records = BTreeMap::new();
            while let Some(key) = map.next_key::<String>()? {
                identifier(&key, 64).map_err(M::Error::custom)?;
                if records.len() >= MAXIMUM_NATIVE_ADDONS || records.contains_key(&key) {
                    return Err(M::Error::custom("duplicate or excessive addon records"));
                }
                let value: NativeAddonRecord = map.next_value()?;
                if value.id != key {
                    return Err(M::Error::custom("addon record identity mismatch"));
                }
                records.insert(key, value);
            }
            Ok(records)
        }
    }
    deserializer.deserialize_map(Records)
}
