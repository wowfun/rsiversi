use super::{
    Controller, ProfileCompiler, ProfileEnvironment, ProfileError, ProfileLimits, ProfileProgram,
    ProfileResolver, ReloadOutcome, Result,
};
use std::fmt;
use std::sync::{Arc, atomic::Ordering};
use tokio::sync::{mpsc, oneshot, watch};

/// A complete immutable composition input; its resolver retains executable provenance.
#[derive(Clone)]
pub struct ProfileInput {
    pub(super) resolver: Arc<dyn ProfileResolver>,
    pub(super) program: ProfileProgram,
    pub(super) compiler: ProfileCompiler,
}

impl fmt::Debug for ProfileInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileInput")
            .finish_non_exhaustive()
    }
}

impl ProfileInput {
    /// Checks selected linked factory preparation without Runtime activation or native loading.
    /// Only explicitly deferred plugin identities may be absent from this catalog.
    /// This is a preliminary argument/configuration check, not an activation proof.
    pub fn preflight_linked(
        &self,
        deferred: &std::collections::BTreeSet<crate::PluginId>,
    ) -> Result<()> {
        let candidate = self.compiler.compile(&self.program)?;
        for leaf in candidate.leaves() {
            let resolved = match self.resolver.resolve(leaf.plugin()) {
                Ok(resolved) => resolved,
                Err(ProfileError::UnknownPlugin { ref plugin, .. })
                    if deferred.contains(plugin) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            if !matches!(
                resolved.identity(),
                rsi_meta::FactoryIdentity::Linked { .. }
            ) {
                continue;
            }
            let (_, _, factory) = resolved.into_parts();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                factory.prepare(leaf.config()).map(drop)
            }))
            .ok()
            .and_then(std::result::Result::ok)
            .ok_or_else(|| ProfileError::Preparation {
                instance: leaf.id().clone(),
            })?;
        }
        Ok(())
    }
    /// Captures trusted immutable inputs without compiling sources or starting work.
    pub fn new(
        resolver: Arc<dyn ProfileResolver>,
        program: ProfileProgram,
        environment: ProfileEnvironment,
        limits: ProfileLimits,
    ) -> Self {
        Self {
            resolver,
            program,
            compiler: ProfileCompiler::new(environment, limits),
        }
    }

    fn validate_successor(&self, successor: &Self) -> Result<()> {
        if self.compiler.environment != successor.compiler.environment
            || self.compiler.limits != successor.compiler.limits
        {
            return Err(ProfileError::IncompatibleInput(
                "environment and limits must remain unchanged".to_owned(),
            ));
        }
        self.resolver
            .validate_successor(successor.resolver.as_ref())
    }
}

/// Owner-only authority for replacing a Profile input in its existing Context.
#[derive(Clone, Debug)]
pub struct ProfileUpdateHandle(Arc<Controller>);

impl ProfileUpdateHandle {
    pub(super) fn new(controller: Arc<Controller>) -> Self {
        Self(controller)
    }

    /// Returns the current input revision, independently of graph convergence revisions.
    ///
    /// # Panics
    /// Panics if the Profile input mutex was poisoned by an earlier panic.
    pub fn input_revision(&self) -> u64 {
        self.0.input.lock().expect("Profile input poisoned").0
    }

    /// Admits one bounded command. Dropping the returned ticket does not cancel it.
    /// Revision and compatibility checks execute in command order before Runtime mutation.
    pub fn submit(
        &self,
        expected_input_revision: u64,
        input: ProfileInput,
    ) -> Result<ProfileUpdateTicket> {
        self.0.submit(Some((expected_input_revision, input)))
    }

    /// Closes admission and joins the executing command before the owner disposes its tree.
    /// This is idempotent; it does not dispose the Profile or create a second lifecycle.
    pub async fn close(&self) {
        self.0.stop().await;
    }

    /// Closes command admission without waiting for convergence. Whole-Runtime
    /// owners use this before Runtime shutdown; Profile cleanup still joins work.
    pub fn close_admission(&self) {
        self.0.accepting.store(false, Ordering::Release);
        self.0.command_stop.cancel();
    }
}

/// One command's completion. It does not own or cancel the execution task.
#[derive(Debug)]
pub struct ProfileUpdateTicket(oneshot::Receiver<Result<ReloadOutcome>>);

impl ProfileUpdateTicket {
    /// Waits for convergence or compensation to complete.
    pub async fn wait(self) -> Result<ReloadOutcome> {
        self.0.await.unwrap_or(Err(ProfileError::Stopped))
    }
}

#[derive(Debug)]
pub(super) struct Command {
    replacement: Option<(u64, ProfileInput)>,
    completion: oneshot::Sender<Result<ReloadOutcome>>,
}

impl Controller {
    pub(super) fn submit(
        &self,
        replacement: Option<(u64, ProfileInput)>,
    ) -> Result<ProfileUpdateTicket> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(ProfileError::Stopped);
        }
        let (completion, receiver) = oneshot::channel();
        self.commands
            .try_send(Command {
                replacement,
                completion,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => ProfileError::Busy,
                mpsc::error::TrySendError::Closed(_) => ProfileError::Stopped,
            })?;
        Ok(ProfileUpdateTicket(receiver))
    }

    pub(super) async fn drive_commands(
        self: Arc<Self>,
        mut receiver: mpsc::Receiver<Command>,
        mut stop: watch::Receiver<bool>,
    ) {
        loop {
            if *stop.borrow() || self.command_stop.is_cancelled() {
                break;
            }
            let command = tokio::select! {
                biased;
                () = self.command_stop.cancelled() => break,
                _ = stop.changed() => break,
                command = receiver.recv() => match command {
                    Some(command) => command,
                    None => break,
                },
            };
            // Only this worker mutates input. The lock also serializes retirement.
            let _reload = self.reload_lock.lock().await;
            let result = if *stop.borrow() || self.command_stop.is_cancelled() {
                Err(ProfileError::Stopped)
            } else {
                self.execute_command(command.replacement).await
            };
            let _ = command.completion.send(result);
        }
        self.accepting.store(false, Ordering::Release);
        receiver.close();
        while let Some(command) = receiver.recv().await {
            let _ = command.completion.send(Err(ProfileError::Stopped));
        }
        // Native child cleanup may wait for factories retained by these inputs.
        // Release them before the parent's deferred effect can be reached.
        self.stop().await;
    }

    async fn execute_command(
        &self,
        replacement: Option<(u64, ProfileInput)>,
    ) -> Result<ReloadOutcome> {
        let (revision, previous) = self.input.lock().expect("Profile input poisoned").clone();
        let previous = previous.ok_or(ProfileError::Stopped)?;
        let Some((expected, candidate)) = replacement else {
            return self.reload_serialized(&previous, &previous, false).await;
        };
        if expected != revision {
            return Err(ProfileError::InputConflict {
                expected,
                current: revision,
            });
        }
        let next = revision.checked_add(1).ok_or_else(|| {
            ProfileError::InvalidProgram("Profile input revision exhausted".to_owned())
        })?;
        previous.validate_successor(&candidate)?;
        let outcome = self.reload_serialized(&candidate, &previous, true).await?;
        if matches!(
            outcome,
            ReloadOutcome::Applied(_) | ReloadOutcome::Unchanged(_)
        ) {
            *self.input.lock().expect("Profile input poisoned") = (next, Some(candidate));
        }
        Ok(outcome)
    }
}
