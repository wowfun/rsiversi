use super::{Candidates, MAX_OBJECTS, MAX_PACKAGES, PackageStore};
use crate::{Error, Result, invalid};
use crate::{InstallSource, Interface};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const DOCUMENT: &str = "profile-roots.json";
type Roots = BTreeMap<String, String>;

/// Newly admitted packages and the profile reservation that keeps them alive.
#[derive(Debug)]
pub struct ProfileAdmission {
    /// Package names produced by the importer and their exact manifest digests.
    pub packages: BTreeMap<String, String>,
    /// Retained root digests and package names after admission.
    pub roots: BTreeMap<String, String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: u32,
    profiles: BTreeMap<String, Roots>,
}

fn validate(roots: &Roots) -> Result<()> {
    if roots.len() > MAX_PACKAGES {
        return Err(invalid("profile root capacity"));
    }
    for (digest, name) in roots {
        crate::hash(digest)?;
        crate::identifier(name)?;
    }
    Ok(())
}
fn profile_key(key: &str) -> Result<()> {
    if let Some((owner, name)) = key.split_once('/') {
        crate::identifier(owner)?;
        crate::identifier(name)
    } else {
        crate::identifier(key)
    }
}

impl PackageStore {
    fn read_profiles(&self) -> Result<Document> {
        let bytes = match self.root.read(DOCUMENT, crate::MAX_DOCUMENT) {
            Ok(bytes) => bytes,
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Document {
                    format: 1,
                    profiles: BTreeMap::new(),
                });
            }
            Err(e) => return Err(e),
        };
        let document: Document =
            serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
        if document.format != 1 || document.profiles.len() > MAX_PACKAGES {
            return Err(invalid("profile root document bounds or format"));
        }
        for (name, roots) in &document.profiles {
            profile_key(name)?;
            validate(roots)?;
        }
        Ok(document)
    }

    /// Reads one profile's exact retained digests and their package names.
    pub fn profile_roots(&self, profile: &str) -> Result<BTreeMap<String, String>> {
        profile_key(profile)?;
        let _writer = self.root.lock("writer.lock", true)?;
        Ok(self
            .read_profiles()?
            .profiles
            .remove(profile)
            .unwrap_or_default())
    }

    /// Publishes a replacement only if the current roots still match the caller's snapshot.
    pub fn replace_profile_roots(
        &self,
        profile: &str,
        expected: &BTreeMap<String, String>,
        next: &BTreeMap<String, String>,
    ) -> Result<()> {
        profile_key(profile)?;
        validate(expected)?;
        validate(next)?;
        let _writer = self.root.lock("writer.lock", false)?;
        let mut document = self.read_profiles()?;
        let current = document.profiles.remove(profile).unwrap_or_default();
        if &current != expected {
            return Err(invalid("profile roots changed"));
        }
        if &current == next {
            return Ok(());
        }
        let selected = self.resolution_unlocked()?;
        {
            let active = self
                .processes
                .lock()
                .map_err(|_| invalid("process lease lock poisoned"))?;
            for (digest, name) in next {
                if current.get(digest) == Some(name) {
                    continue;
                }
                let globally_selected = selected
                    .packages
                    .get(name)
                    .is_some_and(|row| &row.digest == digest);
                let held = active
                    .get(digest)
                    .and_then(std::sync::Weak::upgrade)
                    .is_some_and(|lease| &lease.manifest().name == name);
                let retained = document
                    .profiles
                    .values()
                    .any(|roots| roots.get(digest) == Some(name));
                if !globally_selected && !held && !retained {
                    return Err(invalid("profile root is not selected or leased"));
                }
            }
        }
        if !next.is_empty() {
            self.walk_profile_roots(
                next.iter()
                    .filter(|(digest, name)| current.get(*digest) != Some(*name))
                    .map(|(digest, name)| (digest.clone(), name.clone()))
                    .collect(),
            )?;
        }
        self.write_profile(document, profile, next)
    }
    fn write_profile(&self, mut document: Document, profile: &str, next: &Roots) -> Result<()> {
        validate(next)?;
        if next.is_empty() {
            document.profiles.remove(profile);
        } else {
            document.profiles.insert(profile.to_owned(), next.clone());
        }
        if document.profiles.len() > MAX_PACKAGES {
            return Err(invalid("profile capacity"));
        }
        let bytes = serde_json::to_vec(&document).map_err(|e| invalid(e.to_string()))?;
        if bytes.len() > crate::MAX_DOCUMENT {
            return Err(invalid("profile root document capacity"));
        }
        self.root.write(DOCUMENT, &bytes, true)
    }

    /// Admits local importer output and reserves its closure in one profile.
    /// Other profiles and the CLI retain their own package selections.
    pub fn stage_for_profile(
        &self,
        profile: &str,
        expected: &BTreeMap<String, String>,
        sources: &[InstallSource],
        interfaces: &[Interface],
        target: &str,
        check: impl FnOnce() -> Result<()>,
    ) -> Result<ProfileAdmission> {
        profile_key(profile)?;
        validate(expected)?;
        if sources.is_empty() || sources.len() > MAX_PACKAGES {
            return Err(invalid("installation source count"));
        }
        let _writer = self.root.lock("writer.lock", false)?;
        let document = self.read_profiles()?;
        if document.profiles.get(profile).unwrap_or(&Roots::new()) != expected {
            return Err(invalid("profile roots changed"));
        }
        let global = self.resolution_unlocked()?;
        let candidates = Self::admit_sources(sources, &[], interfaces, target, &global)?;
        let mut previous = self.walk_profile_roots(expected.clone().into_iter().collect())?;
        for lease in self
            .processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter_map(std::sync::Weak::upgrade)
        {
            previous.extend(
                lease
                    .directories
                    .iter()
                    .filter_map(|dir| dir.path.file_name()?.to_str().map(str::to_owned)),
            );
        }
        let mut packages = BTreeMap::new();
        let mut referenced = BTreeSet::new();
        for (name, (digest, manifest, _)) in &candidates {
            packages.insert(name.clone(), digest.clone());
            for edge in &manifest.dependencies {
                if candidates
                    .get(&edge.name)
                    .is_some_and(|(digest, _, _)| digest == &edge.digest)
                {
                    referenced.insert(edge.digest.clone());
                    continue;
                }
                if global
                    .packages
                    .get(&edge.name)
                    .is_some_and(|selected| selected.digest == edge.digest)
                {
                    continue;
                }
                if !previous.contains(&edge.digest)
                    || self.read_object(&edge.digest)?.1.name != edge.name
                {
                    return Err(invalid("profile dependency is not admitted"));
                }
            }
        }
        let mut done = BTreeSet::new();
        for name in candidates.keys() {
            visit_candidate(name, &candidates, &mut BTreeSet::new(), &mut done)?;
        }
        let mut roots = expected.clone();
        roots.extend(
            packages
                .iter()
                .filter(|(_, digest)| !referenced.contains(*digest))
                .map(|(name, digest)| (digest.clone(), name.clone())),
        );
        validate(&roots)?;
        self.publish_objects(&candidates, check, || {
            self.write_profile(document, profile, &roots)?;
            Ok(ProfileAdmission { packages, roots })
        })
    }

    pub(super) fn profile_objects(&self) -> Result<BTreeSet<String>> {
        let pending = self
            .read_profiles()?
            .profiles
            .into_values()
            .flatten()
            .collect();
        self.walk_profile_roots(pending)
    }
    fn walk_profile_roots(&self, mut pending: Vec<(String, String)>) -> Result<BTreeSet<String>> {
        let mut seen = BTreeMap::new();
        while let Some((digest, name)) = pending.pop() {
            if let Some(previous) = seen.get(&digest) {
                if previous != &name {
                    return Err(invalid("profile object has two names"));
                }
                continue;
            }
            if seen.len() >= MAX_OBJECTS {
                return Err(invalid("profile closure capacity"));
            }
            let (_, manifest) = self.read_object(&digest)?;
            if manifest.name != name {
                return Err(invalid("profile object identity differs"));
            }
            seen.insert(digest, name);
            pending.extend(
                manifest
                    .dependencies
                    .into_iter()
                    .map(|dependency| (dependency.digest, dependency.name)),
            );
        }
        Ok(seen.into_keys().collect())
    }
}

fn visit_candidate<'a>(
    name: &'a str,
    candidates: &'a Candidates<'_>,
    active: &mut BTreeSet<&'a str>,
    done: &mut BTreeSet<&'a str>,
) -> Result<()> {
    if done.contains(name) {
        return Ok(());
    }
    if !active.insert(name) {
        return Err(invalid("cyclic package dependency"));
    }
    for edge in &candidates[name].1.dependencies {
        if candidates
            .get(&edge.name)
            .is_some_and(|(digest, _, _)| digest == &edge.digest)
        {
            visit_candidate(&edge.name, candidates, active, done)?;
        }
    }
    active.remove(name);
    done.insert(name);
    Ok(())
}
