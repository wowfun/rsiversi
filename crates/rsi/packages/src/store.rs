use crate::{
    Error, ExportKind, Interface, Manifest, Provenance, Result, SignedRepository, TrustReceipt,
    VerifiedRepository,
    filesystem::{Dir, Lock},
    invalid,
};
use rsi_files_native_fs::EntryKind;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

#[derive(Clone, Copy)]
struct Timing(Option<std::time::Instant>);

impl Timing {
    fn start() -> Self {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        Self(
            (*ENABLED.get_or_init(|| {
                std::env::var_os("RSI_PACKAGE_TIMINGS").as_deref()
                    == Some(std::ffi::OsStr::new("1"))
            }))
            .then(std::time::Instant::now),
        )
    }
    fn mark(&mut self, digest: &str, operation: &str) {
        if let Some(start) = self.0 {
            let now = std::time::Instant::now();
            eprintln!(
                "package_timing {operation} {digest} {} ms",
                now.duration_since(start).as_millis()
            );
            self.0 = Some(now);
        }
    }
}
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
};

type Candidates<'a> = BTreeMap<String, (String, Manifest, &'a InstallSource)>;
const MAX_PACKAGES: usize = 128;
mod profile;
pub use profile::ProfileAdmission;
#[cfg(test)]
#[path = "migration_tests.rs"]
mod migration_tests;
const MAX_OBJECTS: usize = 256;
#[derive(Clone, Copy)]
enum Verification {
    Full,
    Metadata,
}
/// One installed selection and its exact admitted provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    /// Manifest SHA-256.
    pub digest: String,
    /// Download authentication or explicitly local origin.
    pub provenance: Provenance,
}
/// One atomically published package resolution.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Resolution {
    /// Format revision.
    pub format: u32,
    /// Selected names, including exact dependencies.
    pub packages: BTreeMap<String, Selection>,
    /// Repository rollback high-water receipts.
    pub trust: BTreeMap<String, TrustReceipt>,
}
impl Default for Resolution {
    fn default() -> Self {
        Self {
            format: 1,
            packages: BTreeMap::new(),
            trust: BTreeMap::new(),
        }
    }
}
/// One explicit source in an atomic installation transaction.
#[derive(Debug)]
pub struct InstallSource {
    /// Directory containing the declared files.
    pub directory: PathBuf,
    /// Exact externally authenticated or local manifest bytes.
    pub manifest: Vec<u8>,
    /// Distinct source provenance.
    pub provenance: Provenance,
}
/// Non-executing storage scoped to one explicit root.
#[derive(Debug)]
pub struct PackageStore {
    root: Dir,
    objects: Dir,
    leases: Dir,
    processes: std::sync::Mutex<BTreeMap<String, std::sync::Weak<PackageLease>>>,
}
/// Retains a complete closure while callers use exported immutable paths.
#[derive(Debug)]
pub struct PackageLease {
    digest: String,
    manifest: Manifest,
    root: PathBuf,
    dependency_payloads: BTreeMap<(String, String), PathBuf>,
    _files: Vec<Lock>,
    directories: Vec<Dir>,
}
/// Fully resolved immutable arguments, retaining the complete selected closure.
#[derive(Debug)]
pub struct ResolvedProcessExtension {
    /// Exact immutable package whose descriptor was resolved.
    pub package: Arc<PackageLease>,
    /// Absolute working directory.
    pub cwd: PathBuf,
    /// Complete arguments after the externally selected executable.
    pub arguments: Vec<String>,
}
/// An installed payload retaining the entire admitted package closure.
#[derive(Debug)]
pub struct MaterializedPayload {
    package: Arc<PackageLease>,
    path: PathBuf,
}
impl MaterializedPayload {
    /// Immutable tree root; valid while this lease is retained.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Exact selection retaining the tree, including a selected dependency's payload.
    pub fn package(&self) -> &Arc<PackageLease> {
        &self.package
    }
}
impl PartialEq for PackageLease {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root && self.manifest == other.manifest
    }
}
impl Eq for PackageLease {}
impl PackageLease {
    /// Borrows a named payload of an exact direct dependency already pinned by this lease.
    pub fn dependency_payload(
        self: &Arc<Self>,
        package: &str,
        export: &str,
    ) -> Result<MaterializedPayload> {
        let path = self
            .dependency_payloads
            .get(&(package.to_owned(), export.to_owned()))
            .ok_or_else(|| invalid("missing direct dependency payload"))?;
        Ok(MaterializedPayload {
            package: self.clone(),
            path: path.clone(),
        })
    }
    /// Exact immutable manifest identity retained by this lease.
    pub fn digest(&self) -> &str {
        &self.digest
    }
    /// Borrows the selected manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    /// Resolves an admitted named export of the expected role.
    pub fn export(&self, name: &str, kind: ExportKind) -> Result<PathBuf> {
        let export = self
            .manifest
            .exports
            .get(name)
            .filter(|export| export.kind == kind)
            .ok_or_else(|| invalid("missing export or wrong role"))?;
        Ok(if kind == ExportKind::PayloadTree {
            self.root.with_file_name("payloads").join(name)
        } else {
            self.root.join(&export.path)
        })
    }
    /// Pins a payload expanded and verified at installation, without a second cache.
    pub fn payload(self: &Arc<Self>, name: &str) -> Result<MaterializedPayload> {
        Ok(MaterializedPayload {
            package: self.clone(),
            path: self.export(name, ExportKind::PayloadTree)?,
        })
    }
}
impl PackageStore {
    /// Obtains exclusive writer and all generation leases before offline removal.
    pub fn into_purge(self) -> Result<PackagePurge> {
        let mut locks = vec![self.root.lock("writer.lock", false)?];
        self.resolution_unlocked()?;
        for (name, kind) in self.leases.names(MAX_OBJECTS)? {
            crate::hash(&name)?;
            if kind != EntryKind::File {
                return Err(invalid("package lease is not a file"));
            }
            locks.push(self.leases.lock(&name, false)?);
        }
        let purge = PackagePurge {
            store: self,
            _locks: locks,
        };
        purge.visit_objects(false)?;
        Ok(purge)
    }
    /// Opens or creates the owned private store; never launches package code.
    pub fn open(root: &Path) -> Result<Self> {
        let root = Dir::open(root, true)?;
        let objects = root.child(Path::new("objects"), true)?;
        let leases = root.child(Path::new("leases"), true)?;
        Ok(Self {
            root,
            objects,
            leases,
            processes: std::sync::Mutex::default(),
        })
    }
    fn resolution_unlocked(&self) -> Result<Resolution> {
        let bytes = match self.root.read("resolution.json", crate::MAX_DOCUMENT) {
            Ok(bytes) => bytes,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Resolution::default());
            }
            Err(error) => return Err(error),
        };
        let resolution: Resolution =
            serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
        if resolution.format != 1
            || resolution.packages.len() > MAX_PACKAGES
            || resolution.trust.len() > 32
        {
            return Err(invalid("resolution bounds or format"));
        }
        for (name, selected) in &resolution.packages {
            crate::identifier(name)?;
            crate::hash(&selected.digest)?;
            validate_provenance(&selected.provenance, &resolution.trust)?;
        }
        for (name, receipt) in &resolution.trust {
            crate::identifier(name)?;
            crate::hash(&receipt.digest)?;
            if receipt.repository != *name || receipt.sequence == 0 {
                return Err(invalid("invalid trust receipt"));
            }
        }
        Ok(resolution)
    }
    /// Reads the current selected packages under a shared coordinator lease.
    pub fn resolution(&self) -> Result<Resolution> {
        let _writer = self.root.lock("writer.lock", true)?;
        self.resolution_unlocked()
    }
    fn write_resolution(&self, resolution: &Resolution) -> Result<()> {
        self.root.write(
            "resolution.json",
            &serde_json::to_vec(resolution).map_err(|e| invalid(e.to_string()))?,
            true,
        )
    }
    /// Verifies metadata and durably advances rollback protection before downloading payloads.
    pub fn accept_repository(
        &self,
        signed: &SignedRepository,
        key: &[u8; 32],
        authority: &str,
        now: u64,
    ) -> Result<VerifiedRepository> {
        let _writer = self.root.lock("writer.lock", false)?;
        let mut resolution = self.resolution_unlocked()?;
        let verified = signed.verify(key, authority, now, resolution.trust.get(authority))?;
        resolution
            .trust
            .insert(authority.to_owned(), verified.receipt().clone());
        if resolution.trust.len() > 32 {
            return Err(invalid("repository capacity"));
        }
        self.write_resolution(&resolution)?;
        Ok(verified)
    }
    /// Publishes all sources and their exact closure in one resolution transaction.
    pub fn install(
        &self,
        sources: &[InstallSource],
        repositories: &[VerifiedRepository],
        interfaces: &[Interface],
        target: &str,
    ) -> Result<Resolution> {
        self.install_checked(sources, repositories, interfaces, target, || Ok(()))
    }
    fn admit_repository_receipts(
        &self,
        resolution: &mut Resolution,
        repositories: &[VerifiedRepository],
    ) -> Result<()> {
        for repository in repositories {
            let receipt = repository.receipt();
            if let Some(previous) = resolution.trust.get(&receipt.repository)
                && (previous.sequence > receipt.sequence
                    || (previous.sequence == receipt.sequence && previous.digest != receipt.digest))
            {
                return Err(Error::Trust("rollback during publication".into()));
            }
            resolution
                .trust
                .insert(receipt.repository.clone(), receipt.clone());
        }
        if resolution.trust.len() > 32 {
            return Err(invalid("repository capacity"));
        }
        if !repositories.is_empty() {
            self.write_resolution(resolution)?;
        }
        Ok(())
    }
    /// Stages verified objects, then checks the producer fence before selection.
    pub fn install_checked(
        &self,
        sources: &[InstallSource],
        repositories: &[VerifiedRepository],
        interfaces: &[Interface],
        target: &str,
        check: impl FnOnce() -> Result<()>,
    ) -> Result<Resolution> {
        if sources.is_empty() || sources.len() > MAX_PACKAGES {
            return Err(invalid("installation source count"));
        }
        let _writer = self.root.lock("writer.lock", false)?;
        let mut resolution = self.resolution_unlocked()?;
        self.admit_repository_receipts(&mut resolution, repositories)?;
        let candidates =
            Self::admit_sources(sources, repositories, interfaces, target, &resolution)?;
        for (name, (digest, _, source)) in &candidates {
            resolution.packages.insert(
                name.clone(),
                Selection {
                    digest: digest.clone(),
                    provenance: source.provenance.clone(),
                },
            );
        }
        if resolution.packages.len() > MAX_PACKAGES {
            return Err(invalid("selected package capacity"));
        }
        let mut manifests = BTreeMap::new();
        for (name, selected) in &resolution.packages {
            let manifest = if let Some((_, manifest, _)) = candidates.get(name) {
                manifest.clone()
            } else {
                self.read_object(&selected.digest)?.1
            };
            if manifest.name != *name {
                return Err(invalid("selected package name differs"));
            }
            manifests.insert(name.clone(), manifest);
        }
        validate_closure(&resolution, &manifests)?;
        self.publish_objects(&candidates, check, || {
            self.write_resolution(&resolution)?;
            Ok(resolution)
        })
    }
    fn admit_sources<'a>(
        sources: &'a [InstallSource],
        repositories: &[VerifiedRepository],
        interfaces: &[Interface],
        target: &str,
        resolution: &Resolution,
    ) -> Result<Candidates<'a>> {
        let mut names = BTreeSet::new();
        let mut candidates = BTreeMap::new();
        for source in sources {
            let manifest = Manifest::parse(&source.manifest)?;
            if manifest.target != "portable" && manifest.target != target {
                return Err(invalid("unsupported package target"));
            }
            manifest.admit_interfaces(interfaces)?;
            if !names.insert(manifest.name.clone()) {
                return Err(invalid("duplicate transaction name"));
            }
            validate_provenance(&source.provenance, &resolution.trust)?;
            let digest = crate::digest(&source.manifest);
            authorize_manifest(&source.provenance, &manifest, &digest, repositories, target)?;

            candidates.insert(manifest.name.clone(), (digest, manifest, source));
        }
        Ok(candidates)
    }
    fn publish_objects<T>(
        &self,
        candidates: &Candidates<'_>,
        check: impl FnOnce() -> Result<()>,
        publish: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.reserve_installation(candidates)?;
        let mut staged = BTreeSet::new();
        let mut reclaimed_disk = false;
        let publication = (|| {
            for (digest, manifest, source) in candidates.values() {
                match self.objects.directory.child_kind(Path::new(digest)) {
                    Ok(EntryKind::Directory) => match self.verify_object(digest) {
                        Ok(_) => continue,
                        Err(error) => {
                            // Only an unselected, unleased candidate can be repaired in place.
                            if self.collect_unlocked(|name| name == digest)?.is_empty() {
                                return Err(error);
                            }
                        }
                    },
                    Ok(_) => return Err(invalid("non-directory package object")),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.into()),
                }
                staged.insert(digest.as_str());
                if let Err(error) = self.stage(digest, manifest, source) {
                    if reclaimed_disk
                        || !matches!(&error, Error::Io(e) if matches!(e.kind(),
                        std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded))
                    {
                        return Err(error);
                    }
                    let incoming: BTreeSet<_> =
                        candidates.values().map(|(id, _, _)| id.as_str()).collect();
                    let removed =
                        self.collect_unlocked(|name| name == digest || !incoming.contains(name))?;
                    if !removed.iter().any(|name| name != digest && !is_stage(name)) {
                        return Err(error);
                    }
                    reclaimed_disk = true;
                    self.stage(digest, manifest, source)?;
                }
            }
            for (digest, manifest, _) in candidates.values() {
                for (name, export) in &manifest.exports {
                    if matches!(
                        export.kind,
                        ExportKind::ProcessExtension | ExportKind::PackageImporter
                    ) {
                        self.resolve_process_unlocked(
                            digest,
                            manifest,
                            name,
                            "admission",
                            export.kind,
                        )?;
                    }
                }
            }
            check()?;
            publish()
        })();
        if publication.is_err() && !staged.is_empty() {
            // A failed sync may follow the resolution rename. Re-read authority
            // rather than assuming the old selection survived publication.
            self.collect_unlocked(|name| is_stage(name) || staged.contains(name))?;
        }
        publication
    }
    fn read_object(&self, digest: &str) -> Result<(Dir, Manifest)> {
        crate::hash(digest)?;
        let directory = self.objects.child(Path::new(digest), false)?;
        let bytes = directory.read("manifest.json", crate::MAX_DOCUMENT)?;
        if crate::digest(&bytes) != digest {
            return Err(Error::Digest);
        }
        Ok((directory, Manifest::parse(&bytes)?))
    }
    fn verify_object(&self, digest: &str) -> Result<(Dir, Manifest)> {
        let mut timing = Timing::start();
        let (directory, manifest) = self.read_object(digest)?;
        let payloads: BTreeMap<_, _> = manifest
            .exports
            .iter()
            .filter(|(_, export)| export.kind == ExportKind::PayloadTree)
            .map(|(name, export)| {
                (
                    export.path.as_str(),
                    (
                        name.as_str(),
                        export
                            .index_digest
                            .as_deref()
                            .expect("validated payload index"),
                    ),
                )
            })
            .collect();
        let mut trees = Vec::with_capacity(payloads.len());
        for file in &manifest.files {
            if let Some(&(name, index_digest)) = payloads.get(file.path.as_str()) {
                trees.push((name, file, index_digest));
            } else {
                let input = rsi_files_native_fs::open_relative_file_no_follow(
                    &directory.directory,
                    &Path::new("content").join(&file.path),
                )?;
                verify_file(input, file, None)?;
            }
        }
        timing.mark(digest, "content");
        let mut remaining = crate::payload::MAX_PAYLOAD_ENTRIES;
        for (name, file, index_digest) in trees {
            crate::payload::verify(&directory, name, file, index_digest, &mut remaining)?;
        }
        timing.mark(digest, "expanded");
        Ok((directory, manifest))
    }
    fn reserve_installation(&self, candidates: &Candidates<'_>) -> Result<()> {
        let mut bytes = 0;
        let mut incoming = BTreeSet::new();
        for (digest, manifest, _) in candidates.values() {
            incoming.insert(digest.as_str());
            bytes += crate::payload::reserved_bytes(manifest);
        }
        if bytes > crate::MAX_STORE_BYTES {
            return Err(invalid("installed object capacity"));
        }
        let mut existing = Vec::new();
        let mut recovered = false;
        for (name, kind) in self.objects.names(MAX_OBJECTS + MAX_PACKAGES)? {
            if kind != EntryKind::Directory {
                return Err(invalid("non-directory package object"));
            }
            if is_stage(&name) {
                self.remove_object_tree(&name)?;
                recovered = true;
            } else if !incoming.contains(name.as_str()) {
                let manifest = match self.read_object(&name) {
                    Ok((_, manifest)) => manifest,
                    Err(error) => {
                        if self.collect_unlocked(|digest| digest == name)?.is_empty() {
                            return Err(error);
                        }
                        continue;
                    }
                };
                let size = crate::payload::reserved_bytes(&manifest);
                bytes += size;
                existing.push((name, size));
            }
        }
        if recovered {
            self.objects.directory.sync_all()?;
        }
        let mut count = incoming.len() + existing.len();
        if count <= MAX_OBJECTS && bytes <= crate::MAX_STORE_BYTES {
            return Ok(());
        }
        let selected = self.selected_objects()?;
        existing.retain(|(digest, _)| !selected.contains(digest));
        let reclaimable: u64 = existing.iter().map(|(_, size)| size).sum();
        if count - existing.len() > MAX_OBJECTS || bytes - reclaimable > crate::MAX_STORE_BYTES {
            return Err(invalid("installed object capacity"));
        }
        for (digest, size) in existing {
            if self.reclaim_unleased(&digest)? {
                count -= 1;
                bytes -= size;
                if count <= MAX_OBJECTS && bytes <= crate::MAX_STORE_BYTES {
                    break;
                }
            }
        }
        self.objects.directory.sync_all()?;
        if count > MAX_OBJECTS || bytes > crate::MAX_STORE_BYTES {
            return Err(invalid("installed object capacity"));
        }
        Ok(())
    }
    fn stage(&self, digest: &str, manifest: &Manifest, source: &InstallSource) -> Result<()> {
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|e| invalid(e.to_string()))?;
        let name = format!("stage-{}", hex::encode(random));
        let staging = self.objects.child(Path::new(&name), true)?;
        // Intent precedes file creation, so an interrupted stage remains bounded and reclaimable.
        staging.write("manifest.json", &source.manifest, false)?;
        let input = rsi_files_native_fs::open_absolute_directory_no_follow(&source.directory)?;
        let mut remaining = crate::payload::MAX_PAYLOAD_ENTRIES;
        for file in &manifest.files {
            let path = Path::new(&file.path);
            let mut parent = PathBuf::from("content");
            if let Some(relative) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                parent.push(relative);
            }
            let directory = staging.child(&parent, true)?;
            let output = rsi_files_native_fs::create_relative_private_file_no_follow(
                &directory.directory,
                Path::new(path.file_name().expect("admitted file")),
            )?;
            let source = rsi_files_native_fs::open_relative_file_no_follow(&input, path)?;
            if let Some((name, export)) = manifest.exports.iter().find(|(_, export)| {
                export.kind == ExportKind::PayloadTree && export.path == file.path
            }) {
                crate::payload::expand(
                    &staging,
                    name,
                    file,
                    export
                        .index_digest
                        .as_deref()
                        .expect("validated payload index"),
                    source,
                    output,
                    &mut remaining,
                )?;
            } else {
                verify_file(source, file, Some(output))?;
            }
            directory.directory.sync_all()?;
        }
        let mut budget = traversal_budget();
        sync_tree(&staging.directory, 0, &mut budget)?;
        drop(staging);
        self.objects
            .rename(&name, digest, EntryKind::Directory, false)
    }
    /// Pins a selected package and every dependency before returning any exported path.
    pub fn acquire(&self, name: &str) -> Result<PackageLease> {
        let _writer = self.root.lock("writer.lock", true)?;
        let resolution = self.resolution_unlocked()?;
        let selected = resolution
            .packages
            .get(name)
            .ok_or_else(|| invalid("package is not installed"))?;
        self.acquire_unlocked(name, &selected.digest, Verification::Full)
    }
    /// Pins an exact admitted generation, even after its name selects a newer one.
    pub fn acquire_exact(&self, name: &str, digest: &str) -> Result<PackageLease> {
        crate::identifier(name)?;
        crate::hash(digest)?;
        let _writer = self.root.lock("writer.lock", true)?;
        self.acquire_unlocked(name, digest, Verification::Full)
    }
    /// Pins an installed process closure after authenticating its metadata and
    /// admitting payload roots. Content is verified at installation; use ordinary
    /// acquisition to check it again. Exact live process leases may be shared.
    pub fn acquire_for_process(&self, name: &str, digest: &str) -> Result<Arc<PackageLease>> {
        crate::identifier(name)?;
        crate::hash(digest)?;
        let cached = self
            .processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(digest)
            .and_then(std::sync::Weak::upgrade);
        if let Some(lease) = cached {
            if lease.manifest().name != name {
                return Err(invalid("package identity differs from manifest"));
            }
            return Ok(lease);
        }
        let mut timing = Timing::start();
        let _writer = self.root.lock("writer.lock", true)?;
        timing.mark(digest, "lock");
        let lease = Arc::new(self.acquire_unlocked(name, digest, Verification::Metadata)?);
        timing.mark(digest, "closure");
        let mut active = self
            .processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active.retain(|_, item| item.strong_count() > 0);
        active.insert(digest.to_owned(), Arc::downgrade(&lease));
        Ok(lease)
    }
    /// Resolves only an exact leased generation, independent of current selections.
    pub fn process_extension(
        &self,
        name: &str,
        digest: &str,
        export: &str,
        generation: &str,
    ) -> Result<ResolvedProcessExtension> {
        self.resolve_extension(
            name,
            digest,
            export,
            generation,
            ExportKind::ProcessExtension,
        )
    }
    /// Resolves an exact installed importer with its complete process lifetime lease.
    pub fn package_importer(
        &self,
        name: &str,
        digest: &str,
        export: &str,
        generation: &str,
    ) -> Result<ResolvedProcessExtension> {
        self.resolve_extension(
            name,
            digest,
            export,
            generation,
            ExportKind::PackageImporter,
        )
    }
    fn resolve_extension(
        &self,
        name: &str,
        digest: &str,
        export: &str,
        generation: &str,
        kind: ExportKind,
    ) -> Result<ResolvedProcessExtension> {
        let package = self.acquire_for_process(name, digest)?;
        let (cwd, arguments) = self.resolve_process_unlocked(
            package.digest(),
            package.manifest(),
            export,
            generation,
            kind,
        )?;
        Ok(ResolvedProcessExtension {
            package,
            cwd,
            arguments,
        })
    }
    fn resolve_process_unlocked(
        &self,
        digest: &str,
        manifest: &Manifest,
        name: &str,
        generation: &str,
        kind: ExportKind,
    ) -> Result<(PathBuf, Vec<String>)> {
        crate::identifier(name)?;
        if generation.is_empty() || generation.len() > 128 || generation.contains('\0') {
            return Err(invalid("process generation"));
        }
        if !manifest.requires.iter().any(|requirement| {
            requirement.name == "rsi.extension.process" && requirement.version == 1
        }) {
            return Err(invalid(
                "process export requires rsi.extension.process version 1",
            ));
        }
        if kind == ExportKind::PackageImporter
            && !manifest
                .requires
                .contains(&crate::importer_protocol_descriptor())
        {
            return Err(invalid("importer requires rsi.packages.importer version 1"));
        }
        let export = manifest
            .exports
            .get(name)
            .filter(|export| export.kind == kind)
            .ok_or_else(|| invalid("missing process export"))?;
        let (object, _) = self.read_object(digest)?;
        let descriptor = object.read(&format!("content/{}", export.path), 64 * 1024)?;
        let file = manifest
            .files
            .iter()
            .find(|file| file.path == export.path)
            .ok_or_else(|| invalid("missing process descriptor file"))?;
        if hex::encode(Sha256::digest(&descriptor)) != file.digest {
            return Err(Error::Digest);
        }
        let descriptor = crate::ProcessExtension::parse(&descriptor)?;
        let resolve = |location: &crate::PayloadPath, directory: bool| -> Result<PathBuf> {
            let (owner, owner_manifest) = if let Some(package) = &location.package {
                let dependency = manifest
                    .dependencies
                    .iter()
                    .find(|dependency| &dependency.name == package)
                    .ok_or_else(|| invalid("process path is not a direct dependency"))?;
                let (owner, owner_manifest) = self.read_object(&dependency.digest)?;
                if owner_manifest.name != *package {
                    return Err(invalid("process dependency identity"));
                }
                (owner, owner_manifest)
            } else {
                (self.read_object(digest)?.0, manifest.clone())
            };
            owner_manifest
                .exports
                .get(&location.payload)
                .filter(|export| export.kind == ExportKind::PayloadTree)
                .ok_or_else(|| invalid("process path lacks a payload export"))?;
            let tree = owner.child(&Path::new("payloads").join(&location.payload), false)?;
            let relative = Path::new(&location.path);
            if directory || location.path.is_empty() {
                tree.child(relative, false)?;
            } else {
                match rsi_files_native_fs::open_relative_file_no_follow(&tree.directory, relative) {
                    Ok(file) if file.metadata()?.is_file() => {}
                    _ => {
                        tree.child(relative, false)?;
                    }
                }
            }
            Ok(tree.path.join(relative))
        };
        let cwd = resolve(&descriptor.cwd, true)?;
        let arguments = descriptor
            .arguments
            .iter()
            .map(|argument| {
                Ok(match argument {
                    crate::ProcessArgument::Literal { value } => value.clone(),
                    crate::ProcessArgument::Generation => generation.to_owned(),
                    crate::ProcessArgument::Path { location } => resolve(location, false)?
                        .into_os_string()
                        .into_string()
                        .map_err(|_| invalid("process payload path is not UTF-8"))?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((cwd, arguments))
    }
    fn acquire_unlocked(
        &self,
        name: &str,
        digest: &str,
        verification: Verification,
    ) -> Result<PackageLease> {
        let mut pending = vec![(name.to_owned(), digest.to_owned())];
        let mut seen = BTreeSet::new();
        let mut files = Vec::new();
        let mut directories = Vec::new();
        let mut selected_manifest = None;
        let mut dependency_payloads = BTreeMap::new();
        while let Some((name, current_digest)) = pending.pop() {
            if !seen.insert((name.clone(), current_digest.clone())) {
                continue;
            }
            if seen.len() > MAX_OBJECTS {
                return Err(invalid("package closure capacity"));
            }
            files.push(self.leases.lock(&current_digest, true)?);
            let (directory, manifest) = match verification {
                Verification::Full => self.verify_object(&current_digest)?,
                Verification::Metadata => {
                    let (directory, manifest) = self.read_object(&current_digest)?;
                    for (name, export) in &manifest.exports {
                        if export.kind == ExportKind::PayloadTree {
                            directory.child(&Path::new("payloads").join(name), false)?;
                        }
                    }
                    (directory, manifest)
                }
            };
            if manifest.name != name {
                return Err(invalid("package identity differs from manifest"));
            }
            for dependency in &manifest.dependencies {
                pending.push((dependency.name.clone(), dependency.digest.clone()));
            }
            if current_digest == digest {
                selected_manifest = Some(manifest);
            } else if selected_manifest.as_ref().is_some_and(|selected| {
                selected
                    .dependencies
                    .iter()
                    .any(|d| d.name == name && d.digest == current_digest)
            }) {
                for (export, role) in &manifest.exports {
                    if role.kind == ExportKind::PayloadTree {
                        dependency_payloads.insert(
                            (name.clone(), export.clone()),
                            directory.path.join("payloads").join(export),
                        );
                    }
                }
            }
            directories.push(directory);
        }
        Ok(PackageLease {
            digest: digest.to_owned(),
            manifest: selected_manifest.ok_or_else(|| invalid("selected manifest missing"))?,
            dependency_payloads,
            root: self.objects.path.join(digest).join("content"),
            _files: files,
            directories,
        })
    }
    /// Atomically removes names, refusing to break any remaining exact dependency.
    pub fn uninstall(&self, names: &[String]) -> Result<Resolution> {
        if names.is_empty() || names.len() > MAX_PACKAGES {
            return Err(invalid("uninstall name count"));
        }
        let _writer = self.root.lock("writer.lock", false)?;
        let mut resolution = self.resolution_unlocked()?;
        for name in names {
            crate::identifier(name)?;
            if resolution.packages.remove(name).is_none() {
                return Err(invalid(format!("package {name} is not installed")));
            }
        }
        self.validate_resolution(&resolution)?;
        self.root.write(
            "resolution.json",
            &serde_json::to_vec(&resolution).map_err(|e| invalid(e.to_string()))?,
            true,
        )?;
        Ok(resolution)
    }
    /// Reclaims unselected, unleased generations and abandoned installation stages.
    pub fn collect(&self) -> Result<Vec<String>> {
        let _writer = self.root.lock("writer.lock", false)?;
        self.collect_unlocked(|_| true)
    }
    fn selected_objects(&self) -> Result<BTreeSet<String>> {
        let resolution = self.resolution_unlocked()?;
        self.validate_resolution(&resolution)?;
        let mut selected: BTreeSet<_> = resolution
            .packages
            .values()
            .map(|selection| selection.digest.clone())
            .collect();
        selected.extend(self.profile_objects()?);
        Ok(selected)
    }
    fn collect_unlocked(&self, reclaim: impl Fn(&str) -> bool) -> Result<Vec<String>> {
        let selected = self.selected_objects()?;
        let mut removed = Vec::new();
        for (name, kind) in self.objects.names(MAX_OBJECTS + MAX_PACKAGES)? {
            if kind != EntryKind::Directory {
                return Err(invalid("non-directory package object"));
            }
            if selected.contains(name.as_str()) || !reclaim(&name) {
                continue;
            }
            if is_stage(&name) {
                self.remove_object_tree(&name)?;
            } else if !self.reclaim_unleased(&name)? {
                continue;
            }
            removed.push(name);
        }
        if !removed.is_empty() {
            self.objects.directory.sync_all()?;
        }
        Ok(removed)
    }
    fn reclaim_unleased(&self, digest: &str) -> Result<bool> {
        crate::hash(digest).map_err(|_| {
            invalid(format!(
                "unknown object {digest:?}; move it outside objects/ before retrying GC"
            ))
        })?;
        let lease = match self.leases.lock(digest, false) {
            Ok(lease) => lease,
            Err(Error::Busy) => return Ok(false),
            Err(error) => return Err(error),
        };
        self.remove_object_tree(digest)?;
        drop(lease);
        self.leases.directory.remove_child(Path::new(digest))?;
        Ok(true)
    }
    fn remove_object_tree(&self, name: &str) -> Result<()> {
        // Interrupted stages can lack a manifest. Descend only from the retained handle.
        visit_tree(
            &self.objects.directory,
            Path::new(name),
            0,
            &mut traversal_budget(),
            true,
        )
    }
    fn validate_resolution(&self, resolution: &Resolution) -> Result<()> {
        let manifests = resolution
            .packages
            .iter()
            .map(|(name, selection)| {
                let manifest = self.read_object(&selection.digest)?.1;
                if manifest.name != *name {
                    return Err(invalid("selected package name differs"));
                }
                Ok((name.clone(), manifest))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        validate_closure(resolution, &manifests)
    }
}

/// Exclusive offline package removal authority; locks survive payload deletion.
#[derive(Debug)]
pub struct PackagePurge {
    store: PackageStore,
    _locks: Vec<Lock>,
}
impl PackagePurge {
    fn visit_objects(&self, remove: bool) -> Result<()> {
        for (name, kind) in self.store.objects.names(MAX_OBJECTS + MAX_PACKAGES)? {
            if kind != EntryKind::Directory {
                return Err(invalid("package object is not a directory"));
            }
            if !is_stage(&name) {
                crate::hash(&name)?;
            }
            let mut budget = traversal_budget();
            visit_tree(
                &self.store.objects.directory,
                Path::new(&name),
                0,
                &mut budget,
                remove,
            )?;
        }
        Ok(())
    }
    /// Removes known payloads, preserving lease and namespace anchors.
    pub fn purge(&self) -> Result<()> {
        self.visit_objects(true)?;
        self.store.objects.directory.sync_all()?;
        for name in ["resolution.json", profile::DOCUMENT] {
            let path = Path::new(name);
            match self.store.root.directory.child_kind(path) {
                Ok(EntryKind::File) => self.store.root.directory.remove_child(path)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
                _ => return Err(invalid("package state is not an ordinary file")),
            }
        }
        self.store.root.directory.sync_all()?;
        Ok(())
    }
}
fn traversal_budget() -> usize {
    crate::MAX_FILES * 128 + crate::payload::MAX_PAYLOAD_ENTRIES + 260
}
fn sync_tree(
    directory: &rsi_files_native_fs::Directory,
    depth: usize,
    budget: &mut usize,
) -> Result<()> {
    if depth > 128 || *budget == 0 {
        return Err(invalid("publication traversal capacity"));
    }
    *budget -= 1;
    for entry in directory.entries()? {
        let entry = entry?;
        if *budget == 0 {
            return Err(invalid("publication entry capacity"));
        }
        let name = entry.file_name();
        match entry.file_type()? {
            EntryKind::Directory => {
                let child = rsi_files_native_fs::open_relative_directory_no_follow(
                    directory,
                    Path::new(&name),
                )?;
                sync_tree(&child, depth + 1, budget)?;
            }
            EntryKind::File => {
                *budget -= 1;
            }
            _ => return Err(invalid("publication refuses links and special files")),
        }
    }
    directory.sync_all()?;
    Ok(())
}
fn is_stage(name: &str) -> bool {
    name.strip_prefix("stage-").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn visit_tree(
    parent: &rsi_files_native_fs::Directory,
    name: &Path,
    depth: usize,
    budget: &mut usize,
    remove: bool,
) -> Result<()> {
    if depth > 128 || *budget == 0 {
        return Err(invalid("GC traversal capacity"));
    }
    *budget -= 1;
    let directory = rsi_files_native_fs::open_relative_directory_no_follow(parent, name)?;
    for entry in directory.entries()? {
        let entry = entry?;
        if *budget == 0 {
            return Err(invalid("GC entry capacity"));
        }
        let child = entry.file_name();
        match entry.file_type()? {
            EntryKind::Directory => {
                visit_tree(&directory, Path::new(&child), depth + 1, budget, remove)?;
            }
            EntryKind::File => {
                *budget -= 1;
                if remove {
                    directory.remove_child(Path::new(&child))?;
                }
            }
            _ => return Err(invalid("GC refuses links and special files")),
        }
    }
    drop(directory);
    if remove {
        parent.remove_empty_directory(name)?;
    }
    Ok(())
}
fn verify_file(
    mut input: File,
    record: &crate::FileRecord,
    mut output: Option<File>,
) -> Result<()> {
    if !input.metadata()?.is_file() || input.metadata()?.len() != record.bytes {
        return Err(Error::Digest);
    }
    let mut hash = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > record.bytes {
            return Err(Error::Digest);
        }
        hash.update(&buffer[..read]);
        if let Some(output) = &mut output {
            output.write_all(&buffer[..read])?;
        }
    }
    if total != record.bytes || hex::encode(hash.finalize()) != record.digest {
        return Err(Error::Digest);
    }
    if let Some(output) = output {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            output.set_permissions(std::fs::Permissions::from_mode(if record.executable {
                0o500
            } else {
                0o400
            }))?;
        }
        output.sync_all()?;
    }
    Ok(())
}
fn authorize_manifest(
    provenance: &Provenance,
    manifest: &Manifest,
    digest: &str,
    repositories: &[VerifiedRepository],
    target: &str,
) -> Result<()> {
    if let Provenance::Signed {
        repository,
        sequence,
        payload,
    } = provenance
    {
        let authenticated = repositories.iter().any(|verified| {
            let receipt = verified.receipt();
            receipt.repository == *repository
                && receipt.sequence == *sequence
                && receipt.digest == *payload
                && verified
                    .repository()
                    .packages
                    .get(&manifest.name)
                    .and_then(|targets| targets.get(target).or_else(|| targets.get("portable")))
                    .is_some_and(|selected| selected.digest == digest)
        });
        if !authenticated {
            return Err(Error::Trust(
                "manifest is not authorized by verified repository metadata".into(),
            ));
        }
    }
    Ok(())
}

fn validate_provenance(
    provenance: &Provenance,
    trust: &BTreeMap<String, TrustReceipt>,
) -> Result<()> {
    if let Provenance::Signed {
        repository,
        sequence,
        payload,
    } = provenance
    {
        crate::identifier(repository)?;
        crate::hash(payload)?;
        let receipt = trust
            .get(repository)
            .ok_or_else(|| Error::Trust("missing trust receipt".into()))?;
        if *sequence > receipt.sequence
            || (*sequence == receipt.sequence && *payload != receipt.digest)
        {
            return Err(Error::Trust(
                "provenance differs from verified receipt".into(),
            ));
        }
    }
    Ok(())
}
fn validate_closure(resolution: &Resolution, manifests: &BTreeMap<String, Manifest>) -> Result<()> {
    fn visit<'a>(
        name: &'a str,
        resolution: &Resolution,
        manifests: &'a BTreeMap<String, Manifest>,
        active: &mut BTreeSet<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> Result<()> {
        if done.contains(name) {
            return Ok(());
        }
        if !active.insert(name) {
            return Err(invalid("cyclic package dependency"));
        }
        for dependency in &manifests
            .get(name)
            .ok_or_else(|| invalid("missing dependency manifest"))?
            .dependencies
        {
            if resolution
                .packages
                .get(&dependency.name)
                .is_none_or(|value| value.digest != dependency.digest)
            {
                return Err(invalid("dependency closure is not exact"));
            }
            visit(&dependency.name, resolution, manifests, active, done)?;
        }
        active.remove(name);
        done.insert(name);
        Ok(())
    }
    let mut active = BTreeSet::new();
    let mut done = BTreeSet::new();
    for name in manifests.keys() {
        visit(name, resolution, manifests, &mut active, &mut done)?;
    }
    Ok(())
}
