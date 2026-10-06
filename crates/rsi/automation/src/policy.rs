use crate::{AdmissionError, AutomationRule};
use rsi_agent_session_protocol::SessionProtectionScope;
use rsi_api_protocol::CallOrigin;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::{
    collections::BTreeMap,
    fs::File,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationGrant {
    pub device: String,
    pub source: String,
    pub rule: String,
    pub view: bool,
    pub cancel: bool,
    pub resume: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub revision: u64,
    pub rules: BTreeMap<String, Vec<AutomationRule>>,
    pub grants: Vec<AutomationGrant>,
    pub retired: Vec<String>,
}
impl Policy {
    /// # Errors
    /// Rejects malformed values or values exceeding the owning protocol bounds.
    pub fn validate(&self) -> Result<(), AdmissionError> {
        if self.rules.len() > 16
            || self.rules.values().map(Vec::len).sum::<usize>() > crate::protocol::MAXIMUM_RULES
            || self.grants.len() > 1024
            || self.retired.len() > 4096
        {
            return Err(AdmissionError::Capacity);
        }
        let mut retired = std::collections::BTreeSet::new();
        for key in &self.retired {
            let (source, rule) = key
                .split_once(':')
                .ok_or_else(|| AdmissionError::Invalid("invalid retired scope".into()))?;
            crate::protocol::identity(source).map_err(AdmissionError::Invalid)?;
            crate::protocol::identity(rule).map_err(AdmissionError::Invalid)?;
            if !retired.insert(key) {
                return Err(AdmissionError::Conflict);
            }
        }
        for (source, rules) in &self.rules {
            crate::protocol::identity(source).map_err(AdmissionError::Invalid)?;
            let mut ids = std::collections::BTreeSet::new();
            for rule in rules {
                rule.validate().map_err(AdmissionError::Invalid)?;
                SessionProtectionScope::new("automation", format!("{source}:{}", rule.id))
                    .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
                if !ids.insert(&rule.id) || retired.contains(&format!("{source}:{}", rule.id)) {
                    return Err(AdmissionError::Conflict);
                }
            }
        }
        for grant in &self.grants {
            for id in [&grant.device, &grant.source, &grant.rule] {
                crate::protocol::identity(id).map_err(AdmissionError::Invalid)?;
            }
            if grant.device.len() != 32
                || !grant
                    .device
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            {
                return Err(AdmissionError::Invalid(
                    "invalid Device grant identity".into(),
                ));
            }
            if self.rule(&grant.source, &grant.rule).is_none()
                && !self
                    .retired
                    .contains(&format!("{}:{}", grant.source, grant.rule))
            {
                return Err(AdmissionError::Invalid(
                    "grant references unknown rule".into(),
                ));
            }
        }
        Ok(())
    }
    pub fn rule(&self, source: &str, id: &str) -> Option<&AutomationRule> {
        self.rules.get(source)?.iter().find(|r| r.id == id)
    }
}
type PolicyGeneration = (Arc<Policy>, BTreeMap<String, CancellationToken>);

#[derive(Debug)]
pub struct PolicyOwner {
    directory: PathBuf,
    lease: File,
    writer: Mutex<()>,
    intake_writer: Mutex<()>,
    inner: Mutex<PolicyGeneration>,
    fenced: AtomicBool,
    intake: Mutex<(Vec<crate::intake::Rejection>, Option<String>, bool)>,
}
impl PolicyOwner {
    /// # Errors
    /// Fails when the selected resources cannot be validated, exclusively owned or made ready.
    pub fn open(directory: PathBuf) -> Result<Self, AdmissionError> {
        let existed = directory.exists();
        rsi_files_native_fs::create_absolute_directory_no_follow(&directory)
            .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
        #[cfg(unix)]
        if !existed {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let m = std::fs::metadata(&directory)
                .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
            if m.uid() != rustix::process::geteuid().as_raw() || m.mode() & 0o077 != 0 {
                return Err(AdmissionError::Invalid(
                    "policy directory must be private".into(),
                ));
            }
        }
        let lease = crate::private_file(&directory.join(".writer.lock"))?;
        lease.try_lock().map_err(|_| AdmissionError::Capacity)?;
        let path = directory.join("policy.json");
        let policy = if path.exists() {
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > 1024 * 1024
            {
                return Err(AdmissionError::Invalid("invalid policy file".into()));
            }
            serde_json::from_slice::<Policy>(&{
                use std::io::Read;
                let mut data = vec![];
                crate::private_file(&path)?
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut data)
                    .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
                data
            })
            .map_err(|e| AdmissionError::Invalid(e.to_string()))?
        } else {
            Policy::default()
        };
        policy.validate()?;
        let intake = crate::intake::load(&directory);
        Ok(Self {
            intake: Mutex::new((intake.0, intake.1, false)),
            writer: Mutex::new(()),
            intake_writer: Mutex::new(()),
            directory,
            lease,
            inner: Mutex::new((Arc::new(policy), BTreeMap::new())),
            fenced: AtomicBool::new(false),
        })
    }
    /// # Errors
    /// Fails after policy fencing or poisoned ownership bookkeeping.
    pub fn snapshot(&self) -> Result<Arc<Policy>, AdmissionError> {
        Ok(self.lock_generation(|| {})?.0.clone())
    }
    fn lock_generation(
        &self,
        after_check: impl FnOnce(),
    ) -> Result<std::sync::MutexGuard<'_, PolicyGeneration>, AdmissionError> {
        if self.fenced.load(Ordering::Acquire) {
            return Err(AdmissionError::Unavailable);
        }
        after_check();
        let inner = self.inner.lock().map_err(|_| AdmissionError::Unavailable)?;
        if self.fenced.load(Ordering::Acquire) {
            return Err(AdmissionError::Unavailable);
        }
        Ok(inner)
    }
    /// # Errors
    /// Rejects invalid policy, stale revisions or failed and uncertain atomic publication.
    pub fn update(&self, expected: u64, next: Policy) -> Result<Policy, AdmissionError> {
        self.update_with(expected, next, |data| self.publish(data))
    }
    fn update_with(
        &self,
        expected: u64,
        mut next: Policy,
        publish: impl FnOnce(&[u8]) -> Result<(), AdmissionError>,
    ) -> Result<Policy, AdmissionError> {
        if self.fenced.load(Ordering::Acquire) {
            return Err(AdmissionError::Unavailable);
        }
        let _writer = self
            .writer
            .lock()
            .map_err(|_| AdmissionError::Unavailable)?;
        if self.fenced.load(Ordering::Acquire) {
            return Err(AdmissionError::Unavailable);
        }
        let previous = self.snapshot()?;
        if previous.revision != expected {
            return Err(AdmissionError::Conflict);
        }
        for (source, rules) in &next.rules {
            for rule in rules {
                if let Some(old) = previous.rule(source, &rule.id) {
                    let changed = serde_json::to_value(old)
                        .map_err(|e| AdmissionError::Invalid(e.to_string()))?
                        != serde_json::to_value(rule)
                            .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
                    if changed && rule.revision <= old.revision {
                        return Err(AdmissionError::Conflict);
                    }
                }
            }
        }
        for (source, rules) in &previous.rules {
            for rule in rules {
                if next.rule(source, &rule.id).is_none() {
                    let key = format!("{source}:{}", rule.id);
                    if !next.retired.contains(&key) {
                        next.retired.push(key);
                    }
                }
            }
        }
        for key in &previous.retired {
            if !next.retired.contains(key) {
                next.retired.push(key.clone());
            }
        }
        next.revision = expected.checked_add(1).ok_or(AdmissionError::Capacity)?;
        next.validate()?;
        let data = serde_json::to_vec(&next).map_err(|e| AdmissionError::Invalid(e.to_string()))?;
        if data.len() > 1024 * 1024 {
            return Err(AdmissionError::Capacity);
        }
        let publication = publish(&data);
        let mut inner = self.inner.lock().map_err(|_| AdmissionError::Unavailable)?;
        if matches!(publication, Err(AdmissionError::OutcomeUnknown)) {
            self.fenced.store(true, Ordering::Release);
            for token in inner.1.values() {
                token.cancel();
            }
        }
        publication?;
        // Every old lease ends at policy publication; new reads acquire the new generation.
        for token in inner.1.values() {
            token.cancel();
        }
        inner.1.clear();
        inner.0 = Arc::new(next.clone());
        Ok(next)
    }
    fn publish(&self, data: &[u8]) -> Result<(), AdmissionError> {
        let scratch = self.directory.join("policy.pending");
        let mut file = crate::private_file(&scratch)?;
        file.set_len(0)
            .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
        file.write_all(data)
            .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
        file.sync_all()
            .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
        std::fs::rename(&scratch, self.directory.join("policy.json"))
            .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
        File::open(&self.directory)
            .and_then(|f| f.sync_all())
            .map_err(|_| AdmissionError::OutcomeUnknown)?;
        Ok(())
    }
    /// # Errors
    /// Rejects revoked callers, missing grants or unavailable policy ownership.
    pub fn permits(
        &self,
        origin: &CallOrigin,
        source: &str,
        rule: &str,
        operation: &str,
    ) -> Result<CancellationToken, AdmissionError> {
        let mut inner = self.lock_generation(|| {})?;
        if inner.0.rule(source, rule).is_none()
            && !(operation == "view" && inner.0.retired.contains(&format!("{source}:{rule}")))
        {
            return Err(AdmissionError::NotFound);
        }
        match origin {
            CallOrigin::Local => Ok(inner.1.entry("local".into()).or_default().clone()),
            CallOrigin::Device(device) => {
                if device.revoked.is_cancelled() {
                    return Err(AdmissionError::Unauthorized);
                }
                let allowed = inner.0.grants.iter().any(|g| {
                    g.device == device.id.as_str()
                        && g.source == source
                        && g.rule == rule
                        && match operation {
                            "view" => g.view,
                            "cancel" => g.cancel,
                            "resume" => g.resume,
                            _ => false,
                        }
                });
                if !allowed {
                    return Err(AdmissionError::Unauthorized);
                }
                Ok(inner
                    .1
                    .entry(format!("{}:{source}:{rule}", device.id.as_str()))
                    .or_default()
                    .clone())
            }
        }
    }
}
impl Drop for PolicyOwner {
    fn drop(&mut self) {
        if let Ok(inner) = self.inner.lock() {
            for token in inner.1.values() {
                token.cancel();
            }
        }
        let _ = &self.lease;
    }
}
impl rsi_session_protocol::SessionProtection for PolicyOwner {
    fn view(
        &self,
        scope: &SessionProtectionScope,
        origin: &CallOrigin,
    ) -> rsi_session_protocol::Result<CancellationToken> {
        if scope.namespace() != "automation" {
            return Err(rsi_session_protocol::SessionError::Api(
                rsi_api_protocol::ApiError::Unauthorized,
            ));
        }
        let (source, rule) =
            scope
                .key()
                .split_once(':')
                .ok_or(rsi_session_protocol::SessionError::Api(
                    rsi_api_protocol::ApiError::Unauthorized,
                ))?;
        self.permits(origin, source, rule, "view").map_err(|error| {
            rsi_session_protocol::SessionError::Api(match error {
                AdmissionError::Unauthorized | AdmissionError::NotFound => {
                    rsi_api_protocol::ApiError::Unauthorized
                }
                _ => rsi_api_protocol::ApiError::Unavailable,
            })
        })
    }
}

impl PolicyOwner {
    pub(crate) fn reject(
        &self,
        source: Option<&str>,
        delivery: Option<&str>,
        reason: crate::intake::Reason,
    ) {
        let Ok(mut log) = self.intake.lock() else {
            return;
        };
        let safe = |s: &str| crate::protocol::identity(s).is_ok();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string();
        if log.0.len() >= 256 {
            log.0.remove(0);
        }
        log.0.push(crate::intake::Rejection {
            timestamp,
            source: source.filter(|s| safe(s)).map(str::to_owned),
            delivery: delivery.filter(|s| safe(s)).map(str::to_owned),
            reason,
        });
        log.2 = true;
    }
    pub(crate) fn flush_intake(&self) {
        let Ok(_writer) = self.intake_writer.lock() else {
            return;
        };
        let rows = {
            let Ok(mut log) = self.intake.lock() else {
                return;
            };
            if !log.2 {
                return;
            }
            log.2 = false;
            log.0.clone()
        };
        let error = crate::intake::save(&self.directory, &rows).err();
        if let Ok(mut log) = self.intake.lock() {
            if error.is_some() {
                log.2 = true;
            }
            log.1 = error;
        }
    }
    pub(crate) fn intake_diagnostics(&self) -> serde_json::Value {
        let Ok(log) = self.intake.lock() else {
            return serde_json::json!({"error":"log_unavailable"});
        };
        serde_json::json!({"rejections":log.0,"error":log.1})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn permission_reads_keep_the_previous_lease_while_publication_waits_on_disk() {
        let directory = tempfile::tempdir().unwrap();
        let owner =
            std::sync::Arc::new(PolicyOwner::open(directory.path().join("policy")).unwrap());
        let policy = Policy {
            rules: BTreeMap::from([("source".into(), vec![crate::store::tests::rule()])]),
            ..Default::default()
        };
        owner.update(0, policy.clone()).unwrap();
        let lease = owner
            .permits(&CallOrigin::Local, "source", "preview", "view")
            .unwrap();
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let writer = owner.clone();
        let publish = tokio::task::spawn_blocking(move || {
            writer.update_with(1, policy, |data| {
                entered.send(()).unwrap();
                released
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
                writer.publish(data)
            })
        });
        entering.await.unwrap();
        assert!(
            owner.inner.try_lock().is_ok(),
            "disk publication must release the permission lock"
        );
        assert!(
            !owner
                .permits(&CallOrigin::Local, "source", "preview", "view")
                .unwrap()
                .is_cancelled()
        );
        assert!(!lease.is_cancelled());
        release.send(()).unwrap();
        assert_eq!(publish.await.unwrap().unwrap().revision, 2);
        assert!(lease.is_cancelled());
    }
    #[test]
    fn snapshots_share_a_generation_and_unavailability_is_not_denial() {
        use rsi_session_protocol::SessionProtection;
        let tmp = tempfile::tempdir().unwrap();
        let owner = PolicyOwner::open(tmp.path().join("policy")).unwrap();
        let first = owner.snapshot().unwrap();
        assert!(Arc::ptr_eq(&first, &owner.snapshot().unwrap()));
        owner
            .update(
                0,
                Policy {
                    rules: BTreeMap::from([("source".into(), vec![crate::store::tests::rule()])]),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(first.revision, 0);
        assert!(!Arc::ptr_eq(&first, &owner.snapshot().unwrap()));
        let scope = SessionProtectionScope::new("automation", "source:preview").unwrap();
        assert!(matches!(
            owner.update_with(1, (*owner.snapshot().unwrap()).clone(), |_| Err(
                AdmissionError::OutcomeUnknown
            )),
            Err(AdmissionError::OutcomeUnknown)
        ));
        assert!(matches!(
            owner.view(&scope, &CallOrigin::Local),
            Err(rsi_session_protocol::SessionError::Api(
                rsi_api_protocol::ApiError::Unavailable
            ))
        ));
    }
    #[tokio::test]
    async fn unavailable_policy_is_not_an_empty_authorized_list() {
        let tmp = tempfile::tempdir().unwrap();
        let instant = crate::now();
        let ledger = crate::Ledger::open(&tmp.path().join("ledger"), instant).unwrap();
        let policy = Arc::new(PolicyOwner::open(tmp.path().join("policy")).unwrap());
        let rule = crate::store::tests::rule();
        policy
            .update(
                0,
                Policy {
                    rules: BTreeMap::from([("source".into(), vec![rule.clone()])]),
                    ..Default::default()
                },
            )
            .unwrap();
        ledger
            .admit(
                "source".into(),
                "one".into(),
                "a".repeat(64),
                rule,
                crate::store::tests::deployment(1, instant),
                instant,
            )
            .await
            .unwrap();
        let owner =
            crate::AutomationService::new(ledger, policy.clone(), None, BTreeMap::new(), None);
        let request = || rsi_automation_api::Request::List {
            after: "0".into(),
            watermark: None,
            limit: 50,
        };
        let denied = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
            id: rsi_api_protocol::DeviceId::from_bytes([1; 16]),
            revoked: CancellationToken::new(),
        });
        assert!(
            owner.api(denied, request(), None).await.unwrap()["entries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            owner.api(CallOrigin::Local, request(), None).await.unwrap()["entries"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            policy.update_with(1, (*policy.snapshot().unwrap()).clone(), |_| Err(
                AdmissionError::OutcomeUnknown
            )),
            Err(AdmissionError::OutcomeUnknown)
        ));
        assert!(matches!(
            owner.api(CallOrigin::Local, request(), None).await,
            Err(rsi_api_protocol::ApiError::Unavailable)
        ));
        owner.close().await;
    }
    #[tokio::test]
    async fn publication_fence_rejects_a_reader_already_waiting_for_the_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let owner = Arc::new(PolicyOwner::open(tmp.path().join("policy")).unwrap());
        let reader = owner.clone();
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let waiting = tokio::task::spawn_blocking(move || {
            reader
                .lock_generation(|| {
                    entered.send(()).unwrap();
                    released
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                })
                .map(|guard| guard.0.clone())
        });
        entering.await.unwrap();
        assert!(matches!(
            owner.update_with(0, Policy::default(), |_| Err(
                AdmissionError::OutcomeUnknown
            )),
            Err(AdmissionError::OutcomeUnknown)
        ));
        release.send(()).unwrap();
        assert!(
            matches!(waiting.await.unwrap(), Err(AdmissionError::Unavailable)),
            "an earlier fence check must not admit a generation after uncertain publication"
        );
    }
}
