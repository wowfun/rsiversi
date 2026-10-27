use crate::{Result, error, filesystem as fs, path_receipt::PathEdit};
use rsi_files_native_fs as native;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const RECORD: &str = "install.json";
// Stable across journal versions so admission and recovery observe the same transaction.
pub const PENDING: &str = ".rsi-core-upgrade.pending.json";
pub const CORE: &str = if cfg!(windows) { "rsi.exe" } else { "rsi" };
pub const UNINSTALLER: &str = if cfg!(windows) {
    "rsi-uninstall.exe"
} else {
    "rsi-uninstall"
};
pub const LAUNCHER: &str = if cfg!(windows) {
    "uninstall.ps1"
} else {
    "uninstall.sh"
};

#[derive(Clone, Debug)]
pub struct Layout {
    pub prefix: PathBuf,
    pub bin: PathBuf,
    pub control: PathBuf,
}
impl Layout {
    pub fn new(prefix: &Path) -> Result<Self> {
        let prefix = fs::absolute(prefix)?;
        Ok(Self {
            bin: prefix.join("bin"),
            control: prefix.join("libexec/rsi"),
            prefix,
        })
    }
    pub fn target(&self, role: Role) -> PathBuf {
        self.directory(role).join(role.name())
    }
    pub fn directory(&self, role: Role) -> &Path {
        if role == Role::Core {
            &self.bin
        } else {
            &self.control
        }
    }
    pub fn record(&self) -> Result<Option<Record>> {
        let dir = match native::open_absolute_directory_no_follow(&self.control) {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        fs::read(&dir, RECORD, fs::DOCUMENT_LIMIT)?
            .map(|bytes| Record::parse(&bytes, self))
            .transpose()
    }
    pub fn preflight(&self) -> Result<()> {
        if self
            .record()
            .map_err(|e| error(format!("unreadable installation record at {}: {e}; finish uninstall with a trusted external uninstaller and --force before reinstalling", self.target(Role::Record).display())))?
            .is_some_and(|record| record.phase != Phase::Installed)
        {
            return Err(error(format!(
                "incomplete maintenance at {}; finish uninstall before reinstalling",
                self.target(Role::Record).display()
            )));
        }
        for role in Role::ALL {
            let path = self.target(role);
            match std::fs::symlink_metadata(&path) {
                Ok(meta)
                    if role == Role::Core && meta.is_file() && !meta.file_type().is_symlink() =>
                {
                    #[cfg(windows)]
                    let command = format!("& '{}'", path.to_string_lossy().replace('\'', "''"));
                    #[cfg(unix)]
                    let command = format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
                    return Err(error(format!(
                        "rsi is already installed at {}\nUpgrade: {command} core upgrade --local 'ABSOLUTE_CORE_PACKAGE'\n--local accepts a core package directory containing package.json, not a bare executable.\nPackage preparation: docs/user/source-install.md\nChoose another installation directory with {} DIR.",
                        path.display(),
                        if cfg!(windows) { "-Prefix" } else { "--prefix" }
                    )));
                }
                Ok(_) => {
                    return Err(error(format!(
                        "installation destination exists: {}; finish uninstall or choose another prefix",
                        path.display()
                    )));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            fs::absolute(self.directory(role))?;
        }
        Ok(())
    }
    pub fn reject_pending(&self) -> Result<()> {
        match std::fs::symlink_metadata(self.bin.join(PENDING)) {
            Ok(_) => Err(error(
                "core upgrade pending; recover it with its original homes before removal",
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Core,
    Uninstaller,
    Launcher,
    Record,
}
impl Role {
    pub const ALL: [Self; 4] = [Self::Core, Self::Uninstaller, Self::Launcher, Self::Record];
    pub fn name(self) -> &'static str {
        match self {
            Self::Core => CORE,
            Self::Uninstaller => UNINSTALLER,
            Self::Launcher => LAUNCHER,
            Self::Record => RECORD,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Installing,
    Installed,
    Uninstalling,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub format: u8,
    pub id: String,
    pub prefix: PathBuf,
    pub phase: Phase,
    pub core: String,
    pub uninstaller: String,
    pub launcher: String,
    pub path_edits: Vec<PathEdit>,
}
pub fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Record {
    pub fn parse(bytes: &[u8], layout: &Layout) -> Result<Self> {
        if bytes.len() > fs::DOCUMENT_LIMIT {
            return Err(error("installation record exceeds 64 KiB"));
        }
        let record: Self = serde_json::from_slice(bytes).map_err(error)?;
        record.validate(layout)?;
        Ok(record)
    }
    pub fn validate(&self, layout: &Layout) -> Result<()> {
        if self.format != 1
            || !hex(&self.id, 32)
            || self.prefix != layout.prefix
            || ![&self.core, &self.uninstaller, &self.launcher]
                .into_iter()
                .all(|hash| hex(hash, 64))
            || self.path_edits.len() > 3
        {
            return Err(error("unsupported or invalid installation record"));
        }
        for edit in &self.path_edits {
            edit.validate(&self.id, &layout.bin)?;
        }
        Ok(())
    }
    pub fn save(&self, dir: &native::Directory, replace: bool) -> Result<()> {
        fs::write(
            dir,
            RECORD,
            &serde_json::to_vec(self).map_err(error)?,
            replace,
        )
    }
}
