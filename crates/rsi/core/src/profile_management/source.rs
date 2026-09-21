use super::{Arc, Failure, Manager, Principal, Proposal, Reply, Saved, wire};
use crate::{HostLeafEdit, HostProfileId, ProfileCatalog, ProfileEditError, StandardComposition};
use rsi_api_protocol::ApiError;
use rsi_meta_profile::{ProfileControlContract, ProfileHealth, ProfileInstanceState, SnapshotNode};
use sha2::{Digest as _, Sha256};
use std::os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _};

#[derive(Debug)]
pub(super) struct Source {
    pub composition: StandardComposition,
    pub catalog: ProfileCatalog,
    pub local_api: Option<String>,
}
impl Source {
    fn root(&self) -> Result<Option<String>, Failure> {
        let path = self
            .catalog
            .paths()
            .config()
            .join(crate::HOST_PROFILE_DIRECTORY);
        let file = match rsi_files_native_fs::open_absolute_directory_no_follow(&path) {
            Ok(file) => file.into_std_file(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Failure::Source),
        };
        let metadata = file.metadata().map_err(|_| Failure::Source)?;
        let mut digest = Sha256::new();
        digest.update(b"rsi.profile-leaves.root.v1\0");
        digest.update(metadata.dev().to_le_bytes());
        digest.update(metadata.ino().to_le_bytes());
        digest.update(path.as_os_str().as_bytes());
        Ok(Some(hex::encode(digest.finalize())))
    }
    fn require_root(&self, root: &str) -> Result<(), Failure> {
        if self.root()?.as_deref() != Some(root) {
            return Err(Failure::Conflict);
        }
        Ok(())
    }
    fn host(&self) -> Result<Arc<rsi_host::Host>, Failure> {
        let mut composition = self.composition.clone();
        if let Some(staging) = composition.native_staging() {
            composition = composition
                .with_native_staging(staging)
                .map_err(|_| Failure::Source)?;
        }
        composition
            .preview_service(self.local_api.as_deref())
            .map(Arc::new)
            .map_err(|_| Failure::Source)
    }
    fn catalog(
        &self,
        principal: Principal,
        epoch: rsi_api_protocol::HostEpoch,
        request: &wire::CatalogRequest,
    ) -> Result<wire::Catalog, Failure> {
        let root = self.root()?;
        let mut result = wire::Catalog {
            host_epoch: epoch,
            principal,
            root,
            profiles: vec![],
            leaves: vec![],
            next: None,
        };
        let Some(root) = &result.root else {
            return Ok(result);
        };
        if let Some(profile) = &request.profile {
            let id = HostProfileId::new(profile.clone()).map_err(|_| Failure::Source)?;
            let host = self.host()?;
            let bytes = self.catalog.host_edit_source(&id).map_err(failure)?;
            let edit = self
                .catalog
                .preview_host_edit(&host, &id, &bytes)
                .map_err(failure)?;
            leaves(
                edit.effective().proposed.nodes(),
                true,
                root,
                profile,
                &mut result.leaves,
            );
            result
                .leaves
                .sort_by(|a, b| a.target.leaf.cmp(&b.target.leaf));
            result.leaves.retain(|leaf| {
                request
                    .after
                    .as_ref()
                    .is_none_or(|after| &leaf.target.leaf > after)
            });
            let mut bytes = 1024;
            let count = result
                .leaves
                .iter()
                .take(64)
                .take_while(|leaf| {
                    bytes += serde_json::to_vec(leaf).expect("typed leaf").len() + 64;
                    bytes <= 60 * 1024
                })
                .count();
            if result.leaves.len() > count {
                result.next = result
                    .leaves
                    .get(count.saturating_sub(1))
                    .map(|leaf| leaf.target.leaf.clone());
            }
            result.leaves.truncate(count);
        } else {
            result.profiles = self
                .catalog
                .list_hosts()
                .map_err(|_| Failure::Source)?
                .into_iter()
                .filter(|row| matches!(row.source, crate::ProfileSource::User))
                .map(|row| row.id.as_str().to_owned())
                .filter(|id| request.after.as_ref().is_none_or(|after| id > after))
                .collect();
            if result.profiles.len() > 64 {
                result.profiles.truncate(64);
                result.next = result.profiles.last().cloned();
            }
        }
        self.require_root(root)?;
        Ok(result)
    }
    fn prepare(
        &self,
        principal: Principal,
        epoch: rsi_api_protocol::HostEpoch,
        runtime: &rsi_meta::Runtime,
        request: &wire::PreviewRequest,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<Proposal, Failure> {
        self.require_root(&request.target.root)?;
        let host = self.host()?;
        let id = HostProfileId::new(request.target.profile.clone()).map_err(|_| Failure::Source)?;
        let change = match &request.change {
            wire::Change::Enabled { enabled } => HostLeafEdit::Enabled(*enabled),
            wire::Change::Configuration { value } => HostLeafEdit::Configuration(value),
        };
        let edit = self
            .catalog
            .preview_host_leaf_edit(&host, runtime, &id, &request.target.leaf, change)
            .map_err(failure)?;
        let previous = edit
            .effective()
            .previous
            .as_ref()
            .ok_or(Failure::Conflict)?;
        let mut before = vec![];
        leaves(
            previous.nodes(),
            true,
            &request.target.root,
            &request.target.profile,
            &mut before,
        );
        let mut after = vec![];
        leaves(
            edit.effective().proposed.nodes(),
            true,
            &request.target.root,
            &request.target.profile,
            &mut after,
        );
        let before = before
            .iter()
            .find(|leaf| leaf.target == request.target)
            .ok_or(Failure::NotLeaf)?;
        let after = after
            .iter()
            .find(|leaf| leaf.target == request.target)
            .ok_or(Failure::NotLeaf)?;
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| Failure::Source)?;
        let preview = wire::Preview {
            host_epoch: epoch,
            ticket: hex::encode(random),
            target: request.target.clone(),
            operation: request.change.kind(),
            digest: edit.review_digest().into(),
            source_digest: edit.effective().proposed.source_digest().into(),
            plugin: after.plugin.clone(),
            previous_enabled: before.enabled,
            enabled: after.enabled,
            effective_enabled: after.effective_enabled,
        };
        self.require_root(&request.target.root)?;
        let previous_digest = previous.source_digest().to_owned();
        let bytes = edit.proposed_source().to_vec();
        drop(edit);
        Ok(Proposal {
            principal,
            preview,
            previous_digest,
            bytes,
            host,
            _permit: permit,
        })
    }
    fn publish(&self, proposal: Proposal) -> Result<crate::ProfileEditReceipt, Failure> {
        self.require_root(&proposal.preview.target.root)?;
        if self
            .host()?
            .composition_digest()
            .map_err(|_| Failure::Source)?
            != proposal
                .host
                .composition_digest()
                .map_err(|_| Failure::Source)?
        {
            return Err(Failure::Conflict);
        }
        let id =
            HostProfileId::new(proposal.preview.target.profile).map_err(|_| Failure::Source)?;
        let edit = self
            .catalog
            .preview_host_edit(&proposal.host, &id, &proposal.bytes)
            .map_err(failure)?;
        if edit.review_digest() != proposal.preview.digest {
            return Err(Failure::Conflict);
        }
        self.require_root(&proposal.preview.target.root)?;
        edit.commit_once().map_err(failure)
    }
}
fn leaves(
    nodes: &[SnapshotNode],
    parent: bool,
    root: &str,
    profile: &str,
    output: &mut Vec<wire::Leaf>,
) {
    for node in nodes {
        let effective = parent && node.enabled();
        if let Some(plugin) = node.plugin() {
            output.push(wire::Leaf {
                target: wire::Target {
                    root: root.into(),
                    profile: profile.into(),
                    leaf: node.id().into(),
                },
                plugin: plugin.as_str().into(),
                enabled: node.enabled(),
                effective_enabled: effective,
                allowed: vec![],
            });
        }
        leaves(node.children(), effective, root, profile, output);
    }
}
fn failure(error: ProfileEditError) -> Failure {
    match error {
        ProfileEditError::Conflict => Failure::Conflict,
        ProfileEditError::Busy => Failure::Busy,
        ProfileEditError::NotLeaf => Failure::NotLeaf,
        ProfileEditError::DisabledAncestor(parent) => Failure::DisabledAncestor(parent),
        ProfileEditError::Preview(_) => Failure::Preparation,
        _ => Failure::Source,
    }
}

impl Manager {
    pub(super) fn receipts(&self, principal: &Principal) -> Vec<String> {
        self.state
            .lock()
            .expect("Profile receipts")
            .receipts
            .iter()
            .filter(|(_, saved)| &saved.principal == principal)
            .map(|(ticket, _)| ticket.clone())
            .collect()
    }
    pub(super) fn previews(&self, principal: &Principal) -> Vec<wire::Preview> {
        self.state
            .lock()
            .expect("Profile previews")
            .previews
            .values()
            .filter(|proposal| &proposal.principal == principal)
            .map(|proposal| proposal.preview.clone())
            .collect()
    }
    pub(super) async fn catalog(
        &self,
        principal: Principal,
        request: wire::CatalogRequest,
    ) -> Reply<wire::Catalog> {
        request.validate()?;
        let source = self.source.clone();
        let epoch = self.epoch.clone();
        let selected = principal.clone();
        let query = request.clone();
        let result = self
            .context
            .runtime()
            .execution()
            .prepare(move || source.catalog(selected, epoch, &query))
            .await
            .map_err(|_| ApiError::Unavailable)?;
        let mut result = match result {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        for leaf in &mut result.leaves {
            leaf.allowed = self.allowed(&principal, &leaf.target);
        }
        result.validate(&request, &self.epoch)?;
        Ok(Ok(result))
    }
    pub(super) async fn preview(
        &self,
        principal: Principal,
        request: wire::PreviewRequest,
    ) -> Reply<wire::Preview> {
        request.target.validate()?;
        request.change.validate()?;
        let _scope = match self.scope(&principal, &request.target, request.change.kind()) {
            Ok(token) => token,
            Err(error) => return Ok(Err(error)),
        };
        let permit = self
            .previews
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let source = self.source.clone();
        let epoch = self.epoch.clone();
        let runtime = self.context.runtime().clone();
        let result = self
            .context
            .runtime()
            .execution()
            .prepare(move || source.prepare(principal, epoch, &runtime, &request, permit))
            .await
            .map_err(|_| ApiError::Unavailable)?;
        let proposal = match result {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        proposal.preview.validate(&self.epoch)?;
        let preview = proposal.preview.clone();
        self.state
            .lock()
            .expect("Profile preview publication")
            .previews
            .insert(preview.ticket.clone(), proposal);
        Ok(Ok(preview))
    }
    pub(super) async fn commit(
        &self,
        principal: Principal,
        request: wire::Commit,
    ) -> Reply<wire::Receipt> {
        request.validate(&self.epoch)?;
        let _writer = self.writer.try_acquire().map_err(|_| ApiError::Capacity)?;
        // First inspect ownership without consuming the only proposal.
        let preview = {
            let state = self.state.lock().expect("Profile commit");
            if let Some(saved) = state.receipts.get(&request.ticket) {
                if saved.principal != principal || saved.receipt.preview.digest != request.digest {
                    return Err(ApiError::Unauthorized);
                }
                return Ok(Ok(self.observe(saved)));
            }
            let proposal = state
                .previews
                .get(&request.ticket)
                .ok_or(ApiError::Unavailable)?;
            if proposal.principal != principal || proposal.preview.digest != request.digest {
                return Err(ApiError::Unauthorized);
            }
            if state.receipts.len() == 256 {
                return Ok(Err(Failure::Busy));
            }
            proposal.preview.clone()
        };
        let _scope = match self.scope(&principal, &preview.target, preview.operation) {
            Ok(token) => token,
            Err(error) => return Ok(Err(error)),
        };
        let proposal = {
            let mut state = self.state.lock().expect("Profile write admission");
            let proposal = state
                .previews
                .remove(&request.ticket)
                .ok_or(ApiError::Unavailable)?;
            state.receipts.insert(
                request.ticket.clone(),
                Saved {
                    principal,
                    previous_digest: proposal.previous_digest.clone(),
                    receipt: wire::Receipt {
                        preview,
                        outcome: wire::Outcome::Pending,
                    },
                },
            );
            proposal
        };
        let source = self.source.clone();
        let result = self
            .context
            .runtime()
            .execution()
            .prepare(move || source.publish(proposal))
            .await;
        let outcome = match result {
            Ok(Ok(receipt)) => wire::Outcome::Saved {
                directory_synced: receipt.directory_synced,
                application: wire::Application::Pending,
            },
            Ok(Err(failure)) => wire::Outcome::Failed { failure },
            Err(_) => wire::Outcome::Unknown,
        };
        let mut state = self.state.lock().expect("Profile receipt publication");
        let saved = state
            .receipts
            .get_mut(&request.ticket)
            .expect("admitted receipt remains retained");
        saved.receipt.outcome = outcome;
        Ok(Ok(self.observe(saved)))
    }
    pub(super) fn receipt(
        &self,
        principal: &Principal,
        request: &wire::Ticket,
    ) -> Reply<wire::Receipt> {
        request.validate(&self.epoch)?;
        let state = self.state.lock().expect("Profile receipt observation");
        let saved = state
            .receipts
            .get(&request.ticket)
            .ok_or(ApiError::Unavailable)?;
        if &saved.principal != principal {
            return Err(ApiError::Unauthorized);
        }
        Ok(Ok(self.observe(saved)))
    }
    pub(super) fn discard(&self, principal: &Principal, request: &wire::Ticket) -> Reply<()> {
        request.validate(&self.epoch)?;
        let mut state = self.state.lock().expect("Profile preview discard");
        let Some(proposal) = state.previews.get(&request.ticket) else {
            return Err(ApiError::Unavailable);
        };
        if &proposal.principal != principal {
            return Err(ApiError::Unauthorized);
        }
        state.previews.remove(&request.ticket);
        Ok(Ok(()))
    }
    fn observe(&self, saved: &Saved) -> wire::Receipt {
        let mut receipt = saved.receipt.clone();
        if let wire::Outcome::Saved { application, .. } = &mut receipt.outcome {
            *application = self.application(&receipt.preview.source_digest, &saved.previous_digest);
        }
        receipt
    }
    fn application(&self, proposed: &str, previous: &str) -> wire::Application {
        use wire::Application as A;
        let Some(control) = self.context.lookup_local::<ProfileControlContract>() else {
            return A::Unknown;
        };
        let status = control.status();
        if status.source_digest() != proposed && status.source_digest() != previous {
            return A::NotSelected;
        }
        match status.health() {
            ProfileHealth::RestartRequired => A::RestartRequired,
            ProfileHealth::Degraded => A::Failed,
            ProfileHealth::Stopped => A::Unknown,
            ProfileHealth::Converged if status.diagnostic().is_some() => A::Failed,
            ProfileHealth::Converged
                if status.source_digest() == proposed
                    && status.target().iter().all(|target| {
                        status.observed().iter().any(|observed| {
                            observed.id() == target.id()
                                && observed.factory() == target.factory()
                                && matches!(observed.state(), ProfileInstanceState::Active)
                        })
                    }) =>
            {
                A::Applied
            }
            ProfileHealth::Converging | ProfileHealth::Converged => A::Pending,
        }
    }
}
