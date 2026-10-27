//! Select the journal owner without adopting legacy copies into managed installs.
use crate::{Layout, Result, error, transaction::Transaction};
use std::path::{Path, PathBuf};
#[derive(Debug)]
pub struct Upgrade(UpgradeKind);
#[derive(Debug)]
enum UpgradeKind {
    Legacy(crate::legacy::CoreUpgrade),
    Managed(Transaction),
}
impl Upgrade {
    pub fn has_pending(path: &Path) -> Result<bool> {
        crate::legacy::CoreUpgrade::has_pending(path).map_err(error)
    }
    pub fn prepare(
        executable: &Path,
        candidate: &Path,
        digest: &str,
        paths: &rsi_host::HostPaths,
        running: bool,
        package: &rsi_packages::PackageLease,
    ) -> Result<Self> {
        let layout = layout(executable)?;
        if layout.record().map_err(error)?.is_none() {
            return crate::legacy::CoreUpgrade::prepare(
                executable, candidate, digest, paths, running,
            )
            .map(|value| Self(UpgradeKind::Legacy(value)))
            .map_err(error);
        }
        if executable != layout.target(crate::record::Role::Core) {
            return Err(error(
                "managed upgrade requires the recorded bin/rsi executable",
            ));
        }
        let uninstaller = package
            .export("uninstaller", rsi_packages::ExportKind::Executable)
            .map_err(error)?;
        for (export, name) in [
            ("core", crate::record::CORE),
            ("uninstaller", crate::record::UNINSTALLER),
        ] {
            if package
                .manifest()
                .exports
                .get(export)
                .is_none_or(|entry| entry.path != name)
            {
                return Err(error(
                    "managed core package requires root-level platform filenames",
                ));
            }
        }
        let name = crate::record::LAUNCHER;
        if !package
            .manifest()
            .files
            .iter()
            .any(|file| file.path == name)
        {
            return Err(error("managed core package lacks uninstall launcher"));
        }
        let launcher = candidate
            .parent()
            .ok_or_else(|| error("core export has no parent"))?
            .join(name);
        let admitted = |path| {
            package
                .manifest()
                .files
                .iter()
                .find(|file| file.path == path)
                .map(|file| file.digest.as_str())
                .ok_or_else(|| error("missing admitted package file"))
        };
        let hashes = [
            digest,
            admitted(crate::record::UNINSTALLER)?,
            admitted(name)?,
        ];
        Transaction::prepare(
            layout,
            [candidate, &uninstaller, &launcher],
            hashes,
            paths,
            running,
        )
        .map(|value| Self(UpgradeKind::Managed(value)))
        .map_err(error)
    }
    pub fn recover(bin: &Path, paths: &rsi_host::HostPaths) -> Result<Self> {
        let layout = Layout::new(
            bin.parent()
                .ok_or_else(|| error("missing installation prefix"))?,
        )
        .map_err(error)?;
        let directory =
            rsi_files_native_fs::open_absolute_directory_no_follow(bin).map_err(error)?;
        let bytes = crate::filesystem::read(
            &directory,
            crate::record::PENDING,
            crate::filesystem::DOCUMENT_LIMIT,
        )
        .map_err(error)?
        .ok_or_else(|| error("no upgrade is pending"))?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(error)?;
        match value["format"].as_u64() {
            Some(1) => crate::legacy::CoreUpgrade::recover(bin, paths)
                .map(|value| Self(UpgradeKind::Legacy(value)))
                .map_err(error),
            Some(2) => Transaction::recover(layout, paths)
                .map(|value| Self(UpgradeKind::Managed(value)))
                .map_err(error),
            _ => Err(error("unknown or absent core upgrade journal; retained")),
        }
    }
    pub async fn probe_candidates(&self) -> Result<()> {
        match &self.0 {
            UpgradeKind::Legacy(value) => {
                crate::install::probe_candidate(&value.candidate(), false).await
            }
            UpgradeKind::Managed(value) => {
                crate::probe(&value.candidate()).await?;
                crate::probe(&value.candidate_uninstaller()).await
            }
        }
    }
    pub fn executable(&self) -> PathBuf {
        match &self.0 {
            UpgradeKind::Legacy(v) => v.executable(),
            UpgradeKind::Managed(v) => v.executable(),
        }
    }
    pub fn was_running(&self) -> bool {
        match &self.0 {
            UpgradeKind::Legacy(v) => v.was_running(),
            UpgradeKind::Managed(v) => v.was_running(),
        }
    }
    pub fn candidate_digest(&self) -> &str {
        match &self.0 {
            UpgradeKind::Legacy(v) => v.candidate_digest(),
            UpgradeKind::Managed(v) => v.candidate_digest(),
        }
    }
    pub fn promote(&self) -> Result<()> {
        match &self.0 {
            UpgradeKind::Legacy(v) => v.promote(),
            UpgradeKind::Managed(v) => v.promote(),
        }
        .map_err(error)
    }
    pub fn rollback(&self) -> Result<()> {
        match &self.0 {
            UpgradeKind::Legacy(v) => v.rollback(),
            UpgradeKind::Managed(v) => v.rollback(),
        }
        .map_err(error)
    }
    pub fn finish(self, committed: bool) -> Result<PathBuf> {
        match self.0 {
            UpgradeKind::Legacy(v) => v.finish(committed),
            UpgradeKind::Managed(v) => v.finish(committed),
        }
        .map_err(error)
    }
}
fn layout(executable: &Path) -> Result<Layout> {
    Layout::new(
        executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| error("missing executable prefix"))?,
    )
    .map_err(error)
}
