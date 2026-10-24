use crate::{
    Error, ExportKind, Interface, Manifest, Provenance, Result, SignedRepository, TrustReceipt,
    VerifiedRepository, filesystem::Dir, invalid,
};
use rsi_files_native_fs::EntryKind;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

const MAX_PACKAGES: usize = 128;
#[cfg(test)]
#[path = "migration_tests.rs"]
mod migration_tests;
const MAX_OBJECTS: usize = 256;
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
}
/// Retains a complete closure while callers use exported immutable paths.
#[derive(Debug)]
pub struct PackageLease {
    digest: String,
    manifest: Manifest,
    root: PathBuf,
    _files: Vec<File>,
    _directories: Vec<Dir>,
}
impl PartialEq for PackageLease {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root && self.manifest == other.manifest
    }
}
impl Eq for PackageLease {}
impl PackageLease {
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
        Ok(self.root.join(&export.path))
    }
}
impl PackageStore {
    /// Opens or creates the owned private store; never launches package code.
    pub fn open(root: &Path) -> Result<Self> {
        let root = Dir::open(root, true)?;
        let objects = root.child(Path::new("objects"), true)?;
        let leases = root.child(Path::new("leases"), true)?;
        Ok(Self {
            root,
            objects,
            leases,
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
            self.write_resolution(&resolution)?;
        }
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

            resolution.packages.insert(
                manifest.name.clone(),
                Selection {
                    digest: digest.clone(),
                    provenance: source.provenance.clone(),
                },
            );
            candidates.insert(manifest.name.clone(), (digest, manifest, source));
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
        self.collect_unlocked()?;
        let publication = (|| {
            let (mut count, mut bytes) = self.usage()?;
            for (digest, manifest, source) in candidates.values() {
                if match self.objects.directory.child_kind(Path::new(digest)) {
                    Ok(EntryKind::Directory) => true,
                    Ok(_) => return Err(invalid("non-directory package object")),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => return Err(error.into()),
                } {
                    self.verify_object(digest)?;
                    continue;
                }
                count += 1;
                bytes += manifest.files.iter().map(|file| file.bytes).sum::<u64>();
                if count > MAX_OBJECTS || bytes > crate::MAX_STORE_BYTES {
                    return Err(invalid("installed object capacity"));
                }
                self.stage(digest, manifest, source)?;
            }
            check()?;
            self.write_resolution(&resolution)?;
            Ok(resolution)
        })();
        if publication.is_err() {
            // A failed sync may follow the resolution rename. Re-read authority
            // rather than assuming the old selection survived publication.
            self.collect_unlocked()?;
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
        let (directory, manifest) = self.read_object(digest)?;
        for file in &manifest.files {
            let input = rsi_files_native_fs::open_relative_file_no_follow(
                &directory.directory,
                Path::new("content").join(&file.path).as_path(),
            )?;
            verify_file(input, file, None)?;
        }
        Ok((directory, manifest))
    }
    fn usage(&self) -> Result<(usize, u64)> {
        let entries = self.objects.names(MAX_OBJECTS)?;
        let mut bytes = 0_u64;
        for (name, kind) in &entries {
            if *kind != EntryKind::Directory {
                return Err(invalid("non-directory package object"));
            }
            let directory = self.objects.child(Path::new(name), false)?;
            let manifest = Manifest::parse(&directory.read("manifest.json", crate::MAX_DOCUMENT)?)?;
            bytes = bytes
                .checked_add(manifest.files.iter().map(|file| file.bytes).sum::<u64>())
                .ok_or_else(|| invalid("object byte accounting overflow"))?;
        }
        Ok((entries.len(), bytes))
    }
    fn stage(&self, digest: &str, manifest: &Manifest, source: &InstallSource) -> Result<()> {
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|e| invalid(e.to_string()))?;
        let name = format!("stage-{}", hex::encode(random));
        let staging = self.objects.child(Path::new(&name), true)?;
        // Intent precedes file creation, so an interrupted stage remains bounded and reclaimable.
        staging.write("manifest.json", &source.manifest, false)?;
        let input = rsi_files_native_fs::open_absolute_directory_no_follow(&source.directory)?;
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
            verify_file(source, file, Some(output))?;
            directory.directory.sync_all()?;
        }
        let mut budget = crate::MAX_FILES * 128 + 2;
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
        self.acquire_unlocked(name, &selected.digest)
    }
    /// Pins an exact admitted generation, even after its name selects a newer one.
    pub fn acquire_exact(&self, name: &str, digest: &str) -> Result<PackageLease> {
        crate::identifier(name)?;
        crate::hash(digest)?;
        let _writer = self.root.lock("writer.lock", true)?;
        self.acquire_unlocked(name, digest)
    }
    fn acquire_unlocked(&self, name: &str, digest: &str) -> Result<PackageLease> {
        let mut pending = vec![(name.to_owned(), digest.to_owned())];
        let mut seen = BTreeSet::new();
        let mut files = Vec::new();
        let mut directories = Vec::new();
        let mut selected_manifest = None;
        while let Some((name, current_digest)) = pending.pop() {
            if !seen.insert((name.clone(), current_digest.clone())) {
                continue;
            }
            if seen.len() > MAX_OBJECTS {
                return Err(invalid("package closure capacity"));
            }
            files.push(self.leases.lock(&current_digest, true)?);
            let (directory, manifest) = self.verify_object(&current_digest)?;
            if manifest.name != name {
                return Err(invalid("package identity differs from manifest"));
            }
            for dependency in &manifest.dependencies {
                pending.push((dependency.name.clone(), dependency.digest.clone()));
            }
            if current_digest == digest {
                selected_manifest = Some(manifest);
            }
            directories.push(directory);
        }
        Ok(PackageLease {
            digest: digest.to_owned(),
            manifest: selected_manifest.ok_or_else(|| invalid("selected manifest missing"))?,
            root: self.objects.path.join(digest).join("content"),
            _files: files,
            _directories: directories,
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
        self.collect_unlocked()
    }
    fn collect_unlocked(&self) -> Result<Vec<String>> {
        let resolution = self.resolution_unlocked()?;
        self.validate_resolution(&resolution)?;
        let selected: BTreeSet<_> = resolution
            .packages
            .values()
            .map(|selection| selection.digest.as_str())
            .collect();
        let mut removed = Vec::new();
        for (name, kind) in self.objects.names(MAX_OBJECTS + MAX_PACKAGES)? {
            if kind != EntryKind::Directory {
                return Err(invalid("non-directory package object"));
            }
            if selected.contains(name.as_str()) {
                continue;
            }
            let staging = is_stage(&name);
            let lease = if staging {
                None
            } else {
                crate::hash(&name).map_err(|_| {
                    invalid(format!(
                        "unknown object {name:?}; move it outside objects/ before retrying GC"
                    ))
                })?;
                match self.leases.lock(&name, false) {
                    Ok(lease) => Some(lease),
                    Err(Error::Busy) => continue,
                    Err(error) => return Err(error),
                }
            };
            // All operations below descend from the retained objects handle.
            // A staged tree can be incomplete; deletion never trusts its manifest.
            let mut budget = crate::MAX_FILES * 128 + 2;
            remove_tree(&self.objects.directory, Path::new(&name), 0, &mut budget)?;
            if !staging {
                drop(lease);
                self.leases.directory.remove_child(Path::new(&name))?;
            }
            removed.push(name);
        }
        self.objects.directory.sync_all()?;
        Ok(removed)
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

fn remove_tree(
    parent: &rsi_files_native_fs::Directory,
    name: &Path,
    depth: usize,
    budget: &mut usize,
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
            EntryKind::Directory => remove_tree(&directory, Path::new(&child), depth + 1, budget)?,
            EntryKind::File => {
                *budget -= 1;
                directory.remove_child(Path::new(&child))?;
            }
            _ => return Err(invalid("GC refuses links and special files")),
        }
    }
    drop(directory);
    parent.remove_empty_directory(name)?;
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
