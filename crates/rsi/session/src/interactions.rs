use super::{
    Arc, Result, SessionApprovalControl, SessionId, TurnError, TurnService, map_question_error,
    map_turn_error,
};
use futures_util::StreamExt as _;
use rsi_session_protocol::{InteractionRetention, InteractionSnapshot, InteractionStream};
use rsi_user_questions_protocol::{PendingChanges, UserQuestions};

async fn tree_members(turns: &Arc<dyn TurnService>, root: &SessionId) -> Result<Vec<SessionId>> {
    match turns.tree_sessions(root).await {
        Ok(members) => Ok(members),
        Err(TurnError::SessionNotFound(_)) => Ok(Vec::new()),
        Err(error) => Err(map_turn_error(error)),
    }
}

async fn collect(
    retention: &InteractionRetention,
    approvals: &Arc<dyn SessionApprovalControl>,
    questions: Option<&Arc<dyn UserQuestions>>,
    root: &SessionId,
    members: &[SessionId],
) -> Result<InteractionSnapshot> {
    // Reserve before either broker clones its payload. Release unused capacity only
    // after both bounded snapshots have been collected and validated.
    let reservation = retention.reserve_collection()?;
    let (approval_snapshot, question_snapshot) = if members.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        (
            approvals.pending_for_sessions(members).await?,
            match questions {
                Some(questions) => questions
                    .pending_for_sessions(&[root.as_str().to_owned()])
                    .await
                    .map_err(map_question_error)?,
                None => Vec::new(),
            },
        )
    };
    reservation.retain(approval_snapshot, question_snapshot)
}

pub(super) async fn observe(
    root: SessionId,
    tree_root: SessionId,
    turns: Arc<dyn TurnService>,
    approvals: Arc<dyn SessionApprovalControl>,
    questions: Option<Arc<dyn UserQuestions>>,
    retention: InteractionRetention,
) -> Result<InteractionStream> {
    let mut tree = turns
        .watch_tree_membership(&tree_root)
        .map_err(map_turn_error)?;
    let mut members = tree_members(&turns, &root).await?;
    let mut approval_changes = approvals.watch_pending(&members)?;
    let mut question_changes: PendingChanges = match &questions {
        Some(questions) => questions
            .watch_pending(&[root.as_str().to_owned()])
            .map_err(map_question_error)?,
        None => Box::pin(futures_util::stream::pending()),
    };
    let initial = collect(&retention, &approvals, questions.as_ref(), &root, &members).await?;
    Ok(Box::pin(async_stream::try_stream! {
        yield initial;
        loop {
            let membership_changed = tokio::select! {
                next = tree.next() => { if next.is_none() { break; } true }
                next = approval_changes.next() => { if next.is_none() { break; } false }
                next = question_changes.next() => { if next.is_none() { break; } false }
            };
            if membership_changed {
                members = tree_members(&turns, &root).await?;
                approval_changes = approvals.watch_pending(&members)?;
            }
            // The selected revision has been marked before collecting; changes
            // during collection remain pending for the next iteration.
            yield collect(&retention, &approvals, questions.as_ref(), &root, &members).await?;
        }
    }))
}
