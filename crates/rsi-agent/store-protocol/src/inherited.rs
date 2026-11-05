use super::{ForkTurnSelection, SessionHeader, SessionStore, SessionValidationLease, StoreError};

/// One validated parent interval. The source Header owns the exact selected cut.
#[derive(Debug)]
pub struct InheritedInterval {
    /// Session whose immutable origin specifies this interval.
    pub source: SessionHeader,
    /// Verified parent Header at that origin.
    pub parent: SessionHeader,
    /// Keeps this interval readable throughout the consumer's operation.
    pub lease: SessionValidationLease,
}

/// Failure to bind an inherited history read to its original selection.
#[derive(Debug, thiserror::Error)]
pub enum InheritedReadError {
    /// The immutable lineage exceeds the Session depth bound.
    #[error("inherited chain exceeds lineage depth")]
    Depth,
    /// The Header or Fact/control prefix no longer matches the origin.
    #[error("inherited history binding changed")]
    Binding,
    /// A parent Header could not be fingerprinted.
    #[error(transparent)]
    Header(#[from] rsi_agent_session_protocol::SessionError),
    /// Store validation or lookup failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Captures only the immutable inherited intervals of the caller's authorized Header.
pub async fn capture_inherited_intervals(
    store: &dyn SessionStore,
    header: &SessionHeader,
) -> Result<Vec<InheritedInterval>, InheritedReadError> {
    let mut intervals = Vec::new();
    let mut source = header.clone();
    while let Some(origin) = source.history_origin() {
        if intervals.len() >= rsi_agent_session_protocol::MAXIMUM_FORK_HISTORY_DEPTH {
            return Err(InheritedReadError::Depth);
        }
        let lease = store.prepare_session(&origin.parent_session_id).await?;
        let parent = store.header(&origin.parent_session_id).await?;
        let boundary = store
            .resolve_fork_boundary(
                &origin.parent_session_id,
                &origin.point,
                origin.requested_turns.clone(),
            )
            .await?;
        if parent.fingerprint()? != origin.parent_header_fingerprint
            || boundary.resolved_after_seq != origin.resolved_after_seq
            || boundary.resolved_terminal_seq != origin.resolved_terminal_seq
            || boundary.terminal_prefix_sha256 != origin.terminal_prefix_sha256
            || boundary.resolved_terminal_control_seq != origin.resolved_terminal_control_seq
            || boundary.terminal_control_prefix_sha256 != origin.terminal_control_prefix_sha256
            || boundary.effective_turns != origin.effective_turns
        {
            return Err(InheritedReadError::Binding);
        }
        let inherited = matches!(origin.requested_turns, ForkTurnSelection::All);
        intervals.push(InheritedInterval {
            source,
            parent: parent.clone(),
            lease,
        });
        if !inherited {
            break;
        }
        source = parent;
    }
    intervals.reverse();
    Ok(intervals)
}
