use super::*;
use rsi_agent_session_protocol::{
    CommandArguments, CommandRevision, ContributionId, DomainRequestId, SessionCommandDescriptor,
    SessionCommandInvocation, SessionCommandReceipt, SessionCommandsView,
};
use rsi_client::{command_invocation, execute_command_once, query_command_result, slash_command};
use rsi_session_protocol::Result;
use std::sync::atomic::AtomicBool;

#[derive(Debug)]
pub struct Scenario {
    sent: Mutex<Vec<SessionCommandInvocation>>,
    queries: Mutex<Vec<DomainRequestId>>,
    reply: Mutex<Option<SessionCommandReceipt>>,
    missing: AtomicBool,
    fail_query: AtomicBool,
    blocked: AtomicBool,
    active: AtomicUsize,
    release: Semaphore,
}
impl Default for Scenario {
    fn default() -> Self {
        Self {
            sent: Mutex::default(),
            queries: Mutex::default(),
            reply: Mutex::default(),
            missing: AtomicBool::new(false),
            fail_query: AtomicBool::new(false),
            blocked: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            release: Semaphore::new(0),
        }
    }
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Scenario {
    pub fn discover() -> SessionCommandsView {
        SessionCommandsView::new(
            CommandRevision::Draft { revision: 7 },
            vec![
                SessionCommandDescriptor::new(
                    ContributionId::new("fixture.plan").unwrap(),
                    "plan",
                    "Plan arguments",
                    true,
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }
    pub async fn execute(
        &self,
        invocation: SessionCommandInvocation,
    ) -> Result<SessionCommandReceipt> {
        self.sent.lock().unwrap().push(invocation.clone());
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = Active(&self.active);
        if self.blocked.load(Ordering::SeqCst) {
            self.release.acquire().await.unwrap().forget();
        }
        if !self.missing.load(Ordering::SeqCst) {
            *self.reply.lock().unwrap() =
                Some(SessionCommandReceipt::draft_changed(&invocation, "a".repeat(64)).unwrap());
        }
        Err(SessionError::CommandOutcomeUnknown {
            request_id: invocation.request_id,
        })
    }
    pub fn status(&self, id: &DomainRequestId) -> Result<Option<SessionCommandReceipt>> {
        self.queries.lock().unwrap().push(id.clone());
        if self.fail_query.load(Ordering::SeqCst) {
            return Err(SessionError::Capacity);
        }
        Ok(self.reply.lock().unwrap().clone())
    }
}
fn invocation(_handle: &Handle, id: &str) -> SessionCommandInvocation {
    command_invocation(
        &Scenario::discover(),
        "plan",
        CommandArguments::new("on".into()).unwrap(),
        DomainRequestId::new(id).unwrap(),
    )
    .unwrap()
}

pub async fn exact_command_reconciliation(execution: Execution) {
    assert_eq!(slash_command("  /plan on  "), Some(("plan", "on")));
    assert_eq!(slash_command("//plan on"), None);
    let handle = Handle::new("session", false);
    let request = invocation(&handle, "request");
    let receipt = execute_command_once(handle.as_ref(), request.clone())
        .await
        .unwrap();
    assert_eq!(receipt.revision(), CommandRevision::Draft { revision: 8 });
    assert_eq!(
        handle.commands.sent.lock().unwrap().as_slice(),
        std::slice::from_ref(&request)
    );
    assert_eq!(
        handle.commands.queries.lock().unwrap().as_slice(),
        std::slice::from_ref(&request.request_id)
    );
    *handle.commands.reply.lock().unwrap() = None;
    assert!(matches!(
        query_command_result(handle.as_ref(), &request).await,
        Err(SessionError::CommandOutcomeUnknown { .. })
    ));
    handle.commands.fail_query.store(true, Ordering::SeqCst);
    assert!(
        query_command_result(handle.as_ref(), &request)
            .await
            .is_err()
    );
    handle.commands.fail_query.store(false, Ordering::SeqCst);
    let mut forged = request.clone();
    forged.arguments = CommandArguments::new("off".into()).unwrap();
    *handle.commands.reply.lock().unwrap() =
        Some(SessionCommandReceipt::draft_changed(&forged, "a".repeat(64)).unwrap());
    assert!(matches!(
        query_command_result(handle.as_ref(), &request).await,
        Err(SessionError::CommandOutcomeUnknown { .. })
    ));
    assert_eq!(
        handle.commands.sent.lock().unwrap().len(),
        1,
        "absence, failed query and mismatched receipt never replay"
    );
    owned_commands(execution.clone()).await;
    saved_submission(execution).await;
}

async fn saved_submission(execution: Execution) {
    let runtime = Runtime::with_execution(rsi_meta::RuntimeLimits::default(), execution).unwrap();
    let handle = Handle::new("saved-command", false);
    handle.commands.missing.store(true, Ordering::SeqCst);
    install_service(&runtime, vec![handle.clone()]).await;
    let context = runtime.root();
    let fiber = controller(&context, &handle, Sink::new(false), None).await;
    let client = context.lookup_local::<SessionControllerContract>().unwrap();
    let saved = rsi_client::CommandSubmission::default();
    assert!(
        saved
            .try_slash(
                &client,
                "/skill argument",
                DomainRequestId::new("skill").unwrap()
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(handle.commands.sent.lock().unwrap().is_empty());
    assert!(
        saved
            .try_slash(&client, "/plan on", DomainRequestId::new("saved").unwrap())
            .await
            .is_err()
    );
    let frozen = saved.view().pending.unwrap();
    assert!(
        saved
            .try_slash(
                &client,
                "/plan off",
                DomainRequestId::new("replacement").unwrap()
            )
            .await
            .is_err()
    );
    assert_eq!(saved.view().pending.as_ref(), Some(&frozen));
    assert!(fiber.dispose().await.is_clean());
    assert!(saved.refresh(&client).await.is_err());
    assert_eq!(saved.view().pending.as_ref(), Some(&frozen));
    let replacement = controller(&context, &handle, Sink::new(false), None).await;
    let client = context.lookup_local::<SessionControllerContract>().unwrap();
    *handle.commands.reply.lock().unwrap() =
        Some(SessionCommandReceipt::draft_changed(&frozen, "a".repeat(64)).unwrap());
    saved.refresh(&client).await.unwrap();
    assert!(saved.view().pending.is_none());
    assert_eq!(
        saved.view().receipt.unwrap().request_id(),
        &frozen.request_id
    );
    assert_eq!(handle.commands.sent.lock().unwrap().as_slice(), &[frozen]);
    assert!(replacement.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

async fn owned_commands(execution: Execution) {
    let runtime =
        Runtime::with_execution(rsi_meta::RuntimeLimits::default(), execution.clone()).unwrap();
    let handle = Handle::new("commands", false);
    handle.commands.blocked.store(true, Ordering::SeqCst);
    install_service(&runtime, vec![handle.clone()]).await;
    let context = runtime
        .root()
        .isolate_local_fresh::<SessionControllerContract>()
        .unwrap()
        .0;
    let fiber = controller(&context, &handle, Sink::new(false), None).await;
    let controller = context.lookup_local::<SessionControllerContract>().unwrap();
    let view = controller.commands().await.unwrap();
    assert_eq!(view.revision(), CommandRevision::Draft { revision: 7 });
    for index in 0..4 {
        drop(controller.execute_command(invocation(&handle, &format!("request{index}"))));
    }
    until(&execution, || {
        handle.commands.active.load(Ordering::SeqCst) == 4
    })
    .await;
    assert!(matches!(
        controller
            .execute_command(invocation(&handle, "overflow"))
            .await,
        Err(SessionError::Capacity)
    ));
    assert!(matches!(
        controller.commands().await,
        Err(SessionError::Capacity)
    ));
    assert_eq!(handle.commands.sent.lock().unwrap().len(), 4);
    assert!(fiber.dispose().await.is_clean());
    assert_eq!(handle.commands.active.load(Ordering::SeqCst), 0);
    assert!(matches!(
        controller
            .execute_command(invocation(&handle, "retired"))
            .await,
        Err(SessionError::ShuttingDown)
    ));
    assert_eq!(handle.commands.sent.lock().unwrap().len(), 4);
    assert!(runtime.shutdown().await.is_clean());
}
