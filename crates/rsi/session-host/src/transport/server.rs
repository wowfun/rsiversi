use super::*;

#[derive(Debug)]
pub(super) struct PublishedSocket {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl PublishedSocket {
    fn bind(paths: &SessionHostPaths) -> Result<(UnixListener, Self), SessionHostError> {
        create_private_runtime_directory(paths.runtime_directory())?;
        remove_stale_socket_after_failed_probe(paths.socket())?;
        // Keep this name shorter than `host.sock`: a public path at the Unix
        // sockaddr limit must remain publishable. The owner lease serializes
        // publishers; any crash-left socket is still probed before removal.
        let staging = paths.runtime_directory().join(".s");
        remove_stale_socket_after_failed_probe(&staging)?;
        let listener = UnixListener::bind(&staging).map_err(io_error)?;
        let staged_metadata = fs::symlink_metadata(&staging).map_err(io_error)?;
        if !staged_metadata.file_type().is_socket() {
            let _ = fs::remove_file(&staging);
            return Err(SessionHostError::Invalid(
                "staged Session Host endpoint is not a socket".into(),
            ));
        }
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o600)).map_err(io_error)?;
        if let Err(error) = fs::hard_link(&staging, paths.socket()) {
            let _ = fs::remove_file(&staging);
            return Err(io_error(error));
        }
        let published = fs::symlink_metadata(paths.socket()).map_err(io_error)?;
        let _ = fs::remove_file(&staging);
        if !published.file_type().is_socket()
            || published.dev() != staged_metadata.dev()
            || published.ino() != staged_metadata.ino()
        {
            return Err(SessionHostError::Invalid(
                "published Session Host endpoint changed during staged bind".into(),
            ));
        }
        Ok((
            listener,
            Self {
                path: paths.socket().to_owned(),
                device: published.dev(),
                inode: published.ino(),
            },
        ))
    }
}

impl Drop for PublishedSocket {
    fn drop(&mut self) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        let tombstone = parent.join(format!(".host.cleanup.{}.sock", self.inode));
        if fs::rename(&self.path, &tombstone).is_err() {
            return;
        }
        let matches = fs::symlink_metadata(&tombstone).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        });
        if matches {
            let _ = fs::remove_file(tombstone);
        } else if !self.path.exists() {
            let _ = fs::rename(tombstone, &self.path);
        }
    }
}

/// Same-user bounded, closed-shape framed-JSON server for one Host generation.
#[derive(Debug)]
pub struct UdsSessionServer {
    listener: UnixListener,
    published: PublishedSocket,
    application: Arc<dyn SessionApplication>,
    unpublished_drafts: Arc<UnpublishedDrafts>,
    draft_admission: Arc<Semaphore>,
    frame_budget: FrameReadBudget,
    upload_budget: Arc<Semaphore>,
    launch_key: String,
    host_epoch: HostEpoch,
    expected_uid: u32,
    diagnostics: SessionHostDiagnostics,
}

impl UdsSessionServer {
    /// Stages and atomically publishes one private daemon endpoint.
    pub fn bind(
        paths: &SessionHostPaths,
        application: Arc<dyn SessionApplication>,
        launch_key: impl Into<String>,
        host_epoch: HostEpoch,
    ) -> Result<Self, SessionHostError> {
        let _product_build = session_host_product_build()?;
        paths.validate_daemon_endpoint()?;
        let launch_key = launch_key.into();
        validate_launch_key(&launch_key)?;
        let (listener, published) = PublishedSocket::bind(paths)?;
        let expected_uid = rustix::process::geteuid().as_raw();
        Ok(Self {
            listener,
            published,
            application,
            unpublished_drafts: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            draft_admission: Arc::new(Semaphore::new(MAXIMUM_UNPUBLISHED_DRAFTS)),
            frame_budget: FrameReadBudget::default(),
            upload_budget: Arc::new(Semaphore::new(MAXIMUM_SESSION_INPUT_IMAGE_BYTES)),
            launch_key,
            host_epoch,
            expected_uid,
            diagnostics: SessionHostDiagnostics::default(),
        })
    }

    /// Shares the monotonic diagnostics for this server generation.
    pub fn diagnostics(&self) -> SessionHostDiagnostics {
        self.diagnostics.clone()
    }

    /// Accepts bounded same-user clients until cancellation, then drains for at most 60 seconds.
    pub async fn serve(self, cancellation: CancellationToken) -> Result<(), SessionHostError> {
        let permits = Arc::new(Semaphore::new(MAXIMUM_CONNECTIONS));
        let mut tasks = JoinSet::new();
        let mut draft_sweep = tokio::time::interval_at(
            tokio::time::Instant::now() + UNPUBLISHED_DRAFT_SWEEP_INTERVAL,
            UNPUBLISHED_DRAFT_SWEEP_INTERVAL,
        );
        draft_sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let accepted = tokio::select! {
                biased;
                () = cancellation.cancelled() => break,
                _ = draft_sweep.tick() => {
                    prune_expired_drafts(&self.unpublished_drafts).await;
                    continue;
                }
                completed = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(completed) = completed {
                        record_connection_completion(&self.diagnostics, completed);
                    }
                    continue;
                },
                accepted = self.listener.accept() => accepted,
            };
            let Ok((stream, _)) = accepted else {
                // Transient accept failures (ECONNABORTED, EMFILE/ENFILE
                // under churn, EINTR) must not kill a long-lived daemon.
                self.diagnostics.accept_error();
                tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                continue;
            };
            self.diagnostics.accepted_connection();
            let Ok(credentials) = stream.peer_cred() else {
                self.diagnostics.peer_credential_error();
                continue;
            };
            if !peer_belongs_to_effective_user(credentials.uid(), self.expected_uid) {
                self.diagnostics.foreign_uid_rejection();
                continue;
            }
            let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                self.diagnostics.capacity_rejection();
                continue;
            };
            let application = Arc::clone(&self.application);
            let drafts = Arc::clone(&self.unpublished_drafts);
            let draft_admission = Arc::clone(&self.draft_admission);
            let frame_budget = self.frame_budget.clone();
            let upload_budget = Arc::clone(&self.upload_budget);
            let launch_key = self.launch_key.clone();
            let host_epoch = self.host_epoch.clone();
            let shutdown = cancellation.clone();
            let diagnostics = self.diagnostics.clone();
            tasks.spawn(async move {
                let _permit = permit;
                handle_connection(
                    stream,
                    ConnectionContext {
                        application,
                        drafts,
                        draft_admission,
                        frame_budget,
                        upload_budget,
                        launch_key,
                        host_epoch,
                        shutdown,
                        diagnostics,
                    },
                )
                .await
            });
        }
        drop(self.listener);
        let drain_deadline = tokio::time::Instant::now() + SESSION_HOST_DRAIN_TIMEOUT;
        while !tasks.is_empty() {
            match tokio::time::timeout_at(drain_deadline, tasks.join_next()).await {
                Ok(Some(completed)) => {
                    record_connection_completion(&self.diagnostics, completed);
                }
                Ok(None) => break,
                Err(_) => {
                    tasks.abort_all();
                    let mut aborted = 0;
                    while let Some(completed) = tasks.join_next().await {
                        if completed
                            .as_ref()
                            .is_err_and(tokio::task::JoinError::is_cancelled)
                        {
                            aborted += 1;
                        } else {
                            record_connection_completion(&self.diagnostics, completed);
                        }
                    }
                    self.diagnostics.drain_aborted_connections(aborted);
                    break;
                }
            }
        }
        drop(self.published);
        Ok(())
    }
}

pub(super) struct ConnectionContext {
    application: Arc<dyn SessionApplication>,
    drafts: Arc<UnpublishedDrafts>,
    draft_admission: Arc<Semaphore>,
    frame_budget: FrameReadBudget,
    upload_budget: Arc<Semaphore>,
    launch_key: String,
    host_epoch: HostEpoch,
    shutdown: CancellationToken,
    diagnostics: SessionHostDiagnostics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConnectionFailureStage {
    Handshake,
    Request,
    Response,
}

pub(super) fn record_connection_completion(
    diagnostics: &SessionHostDiagnostics,
    completed: Result<Result<(), ConnectionFailureStage>, tokio::task::JoinError>,
) {
    match completed {
        Ok(Err(ConnectionFailureStage::Handshake)) => diagnostics.handshake_failure(),
        Ok(Err(ConnectionFailureStage::Request)) => diagnostics.request_failure(),
        Ok(Err(ConnectionFailureStage::Response)) => diagnostics.response_failure(),
        Err(error) if error.is_panic() => diagnostics.connection_task_panic(),
        Ok(Ok(())) | Err(_) => {}
    }
}

pub(super) async fn handle_connection(
    mut stream: UnixStream,
    context: ConnectionContext,
) -> Result<(), ConnectionFailureStage> {
    if !negotiate_handshake(&mut stream, &context)
        .await
        .map_err(|_| ConnectionFailureStage::Handshake)?
    {
        return Ok(());
    }
    let (request, _request_admission): (ClientFrame, OwnedSemaphorePermit) =
        read_frame_with_retained_budget(
            &mut stream,
            MAXIMUM_FRAME_BYTES,
            &context.frame_budget,
            REQUEST_READ_TIMEOUT,
            "client request",
        )
        .await
        .map_err(|_| ConnectionFailureStage::Request)?;
    let ClientFrame::Request {
        request_id,
        operation,
    } = request
    else {
        return Err(ConnectionFailureStage::Request);
    };
    validate_request_id(&request_id).map_err(|_| ConnectionFailureStage::Request)?;
    validate_wire_operation(&operation).map_err(|_| ConnectionFailureStage::Request)?;
    if let WireOperation::Subscribe { session_id, cursor } = operation {
        return serve_subscription(
            &mut stream,
            &request_id,
            &session_id,
            ObservationCursor {
                control_seq: cursor.control_seq,
                fact_seq: cursor.fact_seq,
            },
            context.application,
            context.drafts,
            context.shutdown,
        )
        .await
        .map_err(|_| ConnectionFailureStage::Response);
    }
    if matches!(
        &operation,
        WireOperation::ListRecent { .. }
            | WireOperation::History { .. }
            | WireOperation::PendingApprovals { .. }
    ) {
        return serve_sequence(
            &mut stream,
            &request_id,
            context.application,
            context.drafts,
            operation,
        )
        .await
        .map_err(|_| ConnectionFailureStage::Response);
    }
    let uploads = match read_message_uploads(
        &mut stream,
        &request_id,
        &operation,
        &context.frame_budget,
        &context.upload_budget,
    )
    .await
    {
        Ok(uploads) => uploads,
        Err(error) => {
            return write_request_error(&mut stream, request_id, host_as_wire_error(error)).await;
        }
    };
    let result = execute_operation(
        context.application,
        context.drafts,
        context.draft_admission,
        operation,
        uploads,
    )
    .await;
    let (response, error) = match result {
        Ok(response) => (Some(response), None),
        Err(error) => (None, Some(error.into())),
    };
    write_frame(
        &mut stream,
        &ServerFrame::Response {
            request_id,
            response,
            error,
        },
    )
    .await
    .map_err(|_| ConnectionFailureStage::Response)
}

pub(super) async fn write_request_error(
    stream: &mut UnixStream,
    request_id: String,
    error: WireError,
) -> Result<(), ConnectionFailureStage> {
    write_frame(
        stream,
        &ServerFrame::Response {
            request_id,
            response: None,
            error: Some(error),
        },
    )
    .await
    .map_err(|_| ConnectionFailureStage::Response)
}

pub(super) async fn negotiate_handshake(
    stream: &mut UnixStream,
    context: &ConnectionContext,
) -> Result<bool, SessionHostError> {
    let expected_product_build = session_host_product_build()?;
    let hello: ClientFrame = read_frame_with_timeout(
        stream,
        MAXIMUM_HANDSHAKE_FRAME_BYTES,
        &context.frame_budget,
        HANDSHAKE_READ_TIMEOUT,
        "client handshake",
    )
    .await?;
    match hello {
        ClientFrame::Hello {
            protocol_epoch,
            product_build,
            launch_key: requested_key,
            host_epoch: requested_epoch,
        } if protocol_epoch == SESSION_HOST_PROTOCOL_EPOCH
            && product_build == expected_product_build
            && requested_key == context.launch_key
            && requested_epoch == context.host_epoch =>
        {
            write_frame(
                stream,
                &ServerFrame::HelloOk {
                    protocol_epoch: SESSION_HOST_PROTOCOL_EPOCH,
                    product_build: expected_product_build.into(),
                    launch_key: context.launch_key.clone(),
                    host_epoch: context.host_epoch.clone(),
                },
            )
            .await?;
        }
        ClientFrame::Hello { .. } => {
            context.diagnostics.handshake_rejection();
            write_frame(
                stream,
                &ServerFrame::HelloRejected {
                    reason:
                        "protocol epoch, product build, launch key, or Host epoch is incompatible"
                            .into(),
                },
            )
            .await?;
            return Ok(false);
        }
        ClientFrame::Request { .. }
        | ClientFrame::UploadChunk { .. }
        | ClientFrame::UploadEnd { .. } => {
            return Err(SessionHostError::Invalid(
                "the first client frame must be hello".into(),
            ));
        }
    }
    Ok(true)
}

#[derive(Debug)]
pub(super) struct MessageUploads {
    bodies: BTreeMap<u16, Arc<[u8]>>,
    _admission: Option<OwnedSemaphorePermit>,
}

#[allow(clippy::too_many_lines)] // One strict upload decoder owns declaration, chunk order, bounds, and digest verification.
pub(super) async fn read_message_uploads<R>(
    stream: &mut R,
    request_id: &str,
    operation: &WireOperation,
    frame_budget: &FrameReadBudget,
    upload_budget: &Arc<Semaphore>,
) -> Result<MessageUploads, SessionHostError>
where
    R: AsyncRead + Unpin,
{
    let WireOperation::SubmitInput { content, .. } = operation else {
        return Ok(MessageUploads {
            bodies: BTreeMap::new(),
            _admission: None,
        });
    };
    let mut declared = BTreeMap::new();
    let mut total = 0_usize;
    for block in content {
        let WireInputBlock::Image {
            upload_id,
            bytes,
            sha256,
        } = block
        else {
            continue;
        };
        let bytes = usize::try_from(*bytes)
            .map_err(|_| SessionHostError::Invalid("image upload length is unsupported".into()))?;
        if bytes == 0
            || bytes > MAXIMUM_SESSION_INPUT_IMAGE_BYTES
            || sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || declared
                .insert(*upload_id, (bytes, sha256.clone()))
                .is_some()
        {
            return Err(SessionHostError::Invalid(
                "image upload declaration is invalid or duplicated".into(),
            ));
        }
        total = total
            .checked_add(bytes)
            .ok_or_else(|| SessionHostError::Invalid("image upload bytes overflowed".into()))?;
    }
    if total == 0 {
        return Ok(MessageUploads {
            bodies: BTreeMap::new(),
            _admission: None,
        });
    }
    if total > MAXIMUM_SESSION_INPUT_IMAGE_BYTES {
        return Err(SessionHostError::Invalid(format!(
            "image uploads exceed {MAXIMUM_SESSION_INPUT_IMAGE_BYTES} aggregate bytes"
        )));
    }
    let permits = u32::try_from(total)
        .map_err(|_| SessionHostError::Invalid("image upload admission is unsupported".into()))?;
    let admission = tokio::time::timeout(
        REQUEST_READ_TIMEOUT,
        Arc::clone(upload_budget).acquire_many_owned(permits),
    )
    .await
    .map_err(|_| SessionHostError::Io("Session Host image upload admission timed out".into()))?
    .map_err(|_| SessionHostError::Io("Session Host image upload admission closed".into()))?;
    let mut bodies = declared
        .iter()
        .map(|(id, (bytes, _))| (*id, (Vec::with_capacity(*bytes), 0_u32)))
        .collect::<BTreeMap<_, _>>();
    let upload_deadline = tokio::time::Instant::now() + UPLOAD_READ_TIMEOUT;
    loop {
        let frame: ClientFrame = tokio::time::timeout_at(
            upload_deadline,
            read_frame(stream, MAXIMUM_UPLOAD_FRAME_BYTES, frame_budget),
        )
        .await
        .map_err(|_| SessionHostError::Io("Session Host image upload timed out".into()))??;
        match frame {
            ClientFrame::UploadChunk {
                request_id: frame_request_id,
                upload_id,
                index,
                data,
            } if frame_request_id == request_id => {
                if data.is_empty() || data.len() > MAXIMUM_UPLOAD_CHUNK_BASE64_BYTES {
                    return Err(SessionHostError::Invalid(
                        "image upload chunk is empty or oversized".into(),
                    ));
                }
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(data.as_bytes())
                    .map_err(|_| {
                        SessionHostError::Invalid("image upload base64 is invalid".into())
                    })?;
                if decoded.is_empty() || decoded.len() > MAXIMUM_UPLOAD_CHUNK_BYTES {
                    return Err(SessionHostError::Invalid(
                        "image upload chunk is noncanonical or oversized".into(),
                    ));
                }
                let (body, next_index) = bodies.get_mut(&upload_id).ok_or_else(|| {
                    SessionHostError::Invalid("image upload chunk has an unknown identity".into())
                })?;
                if index != *next_index {
                    return Err(SessionHostError::Invalid(
                        "image upload chunk index is not contiguous".into(),
                    ));
                }
                let expected = declared[&upload_id].0;
                if decoded.len()
                    != expected
                        .saturating_sub(body.len())
                        .min(MAXIMUM_UPLOAD_CHUNK_BYTES)
                {
                    return Err(SessionHostError::Invalid(
                        "image upload chunk length does not match its declared remainder".into(),
                    ));
                }
                body.extend_from_slice(&decoded);
                *next_index = next_index.checked_add(1).ok_or_else(|| {
                    SessionHostError::Invalid("image upload chunk index exhausted".into())
                })?;
            }
            ClientFrame::UploadEnd {
                request_id: frame_request_id,
            } if frame_request_id == request_id => break,
            _ => {
                return Err(SessionHostError::Invalid(
                    "image upload frame does not match its request".into(),
                ));
            }
        }
    }
    let mut verified = BTreeMap::new();
    for (upload_id, (body, _)) in bodies {
        let (expected_bytes, expected_sha256) = &declared[&upload_id];
        if body.len() != *expected_bytes
            || hex::encode(sha2::Sha256::digest(&body)) != *expected_sha256
        {
            return Err(SessionHostError::Invalid(
                "image upload length or digest does not match its declaration".into(),
            ));
        }
        verified.insert(upload_id, Arc::from(body));
    }
    Ok(MessageUploads {
        bodies: verified,
        _admission: Some(admission),
    })
}

#[allow(clippy::too_many_lines)] // The closed wire-operation dispatcher keeps every request mapped at one protocol boundary.
pub(super) async fn execute_operation(
    application: Arc<dyn SessionApplication>,
    drafts: Arc<UnpublishedDrafts>,
    draft_admission: Arc<Semaphore>,
    operation: WireOperation,
    mut uploads: MessageUploads,
) -> rsi_session::Result<WireResponse> {
    match operation {
        WireOperation::Probe => Ok(WireResponse::Ready),
        WireOperation::Create {
            cwd,
            session_id,
            agent_preset_id,
            workspace_trust,
        } => {
            create_draft(
                application,
                drafts,
                draft_admission,
                cwd,
                session_id,
                agent_preset_id,
                workspace_trust,
            )
            .await
        }
        WireOperation::Attach { session_id } | WireOperation::Header { session_id } => {
            let handle = get_handle(&application, &drafts, &session_id).await?;
            Ok(WireResponse::Session {
                header: Box::new(handle.header().await?),
            })
        }
        WireOperation::SubmitInput {
            session_id,
            message_id,
            content,
            model,
            sandbox,
        } => {
            let handle = get_handle(&application, &drafts, &session_id).await?;
            let content = content
                .into_iter()
                .map(|block| match block {
                    WireInputBlock::Text { text } => Ok(SessionInput::Text { text }),
                    WireInputBlock::Image { upload_id, .. } => uploads
                        .bodies
                        .remove(&upload_id)
                        .map(|bytes| SessionInput::Image { bytes })
                        .ok_or_else(|| {
                            SessionApplicationError::Invalid("image upload body is missing".into())
                        }),
                })
                .collect::<rsi_session::Result<Vec<_>>>()?;
            validate_session_input(&content)?;
            let result = handle
                .submit(SubmitInput {
                    message_id,
                    content,
                    model,
                    sandbox,
                })
                .await;
            if result.is_ok()
                || matches!(
                    &result,
                    Err(SessionApplicationError::MessageConflict { .. })
                )
            {
                drafts.lock().await.remove(&session_id);
            }
            let receipt = result?;
            Ok(receipt.into())
        }
        WireOperation::MessageStatus {
            session_id,
            message_id,
        } => {
            let handle = get_handle(&application, &drafts, &session_id).await?;
            Ok(handle.message_status(&message_id).await?.into())
        }
        WireOperation::SubmitImage {
            session_id,
            turn_id,
            model,
            request,
        } => {
            let handle = get_handle(&application, &drafts, &session_id).await?;
            let result = handle
                .generate_image(SubmitDirectImage {
                    turn_id,
                    model,
                    request,
                })
                .await;
            if result.is_ok() || matches!(&result, Err(SessionApplicationError::Conflict { .. })) {
                drafts.lock().await.remove(&session_id);
            }
            let receipt = result?;
            Ok(receipt.into())
        }
        WireOperation::Cancel {
            session_id,
            target,
            reason,
        } => {
            let handle = get_handle(&application, &drafts, &session_id).await?;
            let target = match target {
                WireCancelTarget::Message { message_id } => CancelTarget::Message(message_id),
                WireCancelTarget::Turn { turn_id } => CancelTarget::Turn(turn_id),
            };
            let result = handle.cancel(target, reason).await?;
            Ok(WireResponse::Cancel {
                accepted: result.accepted,
                already_terminal: result.already_terminal,
            })
        }
        WireOperation::AnswerApproval {
            session_id,
            approval_id,
            decision,
        } => {
            let handle = get_handle(&application, &drafts, &session_id).await?;
            Ok(WireResponse::ApprovalAnswer {
                accepted: handle.answer_approval(&approval_id, decision).await?,
            })
        }
        WireOperation::ListRecent { .. }
        | WireOperation::History { .. }
        | WireOperation::PendingApprovals { .. }
        | WireOperation::Subscribe { .. } => unreachable!("sequence handled before dispatch"),
    }
}

pub(super) async fn create_draft(
    application: Arc<dyn SessionApplication>,
    drafts: Arc<UnpublishedDrafts>,
    draft_admission: Arc<Semaphore>,
    cwd: String,
    session_id: Option<SessionId>,
    agent_preset_id: Option<AgentPresetId>,
    workspace_trust: WorkspaceTrust,
) -> rsi_session::Result<WireResponse> {
    let cwd = PathBuf::from(cwd);
    if !cwd.is_absolute() {
        return Err(SessionApplicationError::Invalid(
            "wire workspace path must be absolute".into(),
        ));
    }
    let expired = {
        let mut drafts = drafts.lock().await;
        take_expired_drafts(&mut drafts, tokio::time::Instant::now())
    };
    drop(expired);
    let admission = draft_admission
        .try_acquire_owned()
        .map_err(|_| SessionApplicationError::Capacity)?;
    let handle = application
        .create(CreateSession {
            cwd,
            session_id,
            agent_preset_id,
            workspace_trust,
        })
        .await?;
    let header = handle.header().await?;
    let mut drafts = drafts.lock().await;
    let expired = take_expired_drafts(&mut drafts, tokio::time::Instant::now());
    if drafts.contains_key(header.session_id()) {
        drop(drafts);
        drop(expired);
        return Err(SessionApplicationError::Invalid(
            "an unpublished draft with this Session identity already exists".into(),
        ));
    }
    drafts.insert(
        header.session_id().clone(),
        UnpublishedDraft {
            handle,
            expires_at: unpublished_draft_deadline(),
            _admission: admission,
        },
    );
    drop(drafts);
    drop(expired);
    Ok(WireResponse::Session {
        header: Box::new(header),
    })
}

pub(super) async fn serve_sequence(
    stream: &mut UnixStream,
    request_id: &str,
    application: Arc<dyn SessionApplication>,
    drafts: Arc<UnpublishedDrafts>,
    operation: WireOperation,
) -> Result<(), SessionHostError> {
    let result: rsi_session::Result<(WireResponse, Vec<WireItem>)> = match operation {
        WireOperation::ListRecent { after, limit } => {
            let cursor = after.map(|cursor| RecentSessionCursor {
                created_at_ms: cursor.created_at_ms,
                session_id: cursor.session_id,
            });
            application
                .list_recent(cursor.as_ref(), limit)
                .await
                .map(|page| {
                    (
                        WireResponse::RecentStart {
                            has_more: page.has_more,
                        },
                        page.sessions
                            .into_iter()
                            .map(|summary| WireItem::Session {
                                header: summary.header,
                            })
                            .collect(),
                    )
                })
        }
        WireOperation::History {
            session_id,
            exclusive_before_seq,
            limit,
        } => match get_handle(&application, &drafts, &session_id).await {
            Ok(handle) => handle
                .history_before(exclusive_before_seq, limit)
                .await
                .map(|page| {
                    (
                        WireResponse::HistoryStart {
                            before_seq: page.before_seq,
                            durable_seq: page.durable_seq,
                            has_more: page.has_more,
                        },
                        page.facts
                            .into_iter()
                            .map(|fact| WireItem::Fact {
                                session_id: session_id.clone(),
                                fact,
                            })
                            .collect(),
                    )
                }),
            Err(error) => Err(error),
        },
        WireOperation::PendingApprovals { session_id } => {
            match get_handle(&application, &drafts, &session_id).await {
                Ok(handle) => handle.pending_approvals().await.map(|requests| {
                    (
                        WireResponse::PendingApprovalsStart,
                        requests
                            .into_iter()
                            .map(|request| WireItem::Approval { request })
                            .collect(),
                    )
                }),
                Err(error) => Err(error),
            }
        }
        _ => unreachable!("only sequence operations reach sequence dispatch"),
    };

    let (response, items) = match result {
        Ok(result) => result,
        Err(error) => {
            return write_frame(
                stream,
                &ServerFrame::Response {
                    request_id: request_id.into(),
                    response: None,
                    error: Some(error.into()),
                },
            )
            .await;
        }
    };
    write_sequence_frames(stream, request_id, response, items).await
}

pub(super) async fn write_sequence_frames(
    stream: &mut UnixStream,
    request_id: &str,
    response: WireResponse,
    items: Vec<WireItem>,
) -> Result<(), SessionHostError> {
    write_frame(
        stream,
        &ServerFrame::Response {
            request_id: request_id.into(),
            response: Some(response),
            error: None,
        },
    )
    .await?;
    for item in items {
        write_frame(
            stream,
            &ServerFrame::Item {
                request_id: request_id.into(),
                item: Box::new(item),
            },
        )
        .await?;
    }
    write_frame(
        stream,
        &ServerFrame::End {
            request_id: request_id.into(),
            error: None,
        },
    )
    .await
}

impl From<TurnReceipt> for WireResponse {
    fn from(receipt: TurnReceipt) -> Self {
        Self::TurnReceipt {
            session_id: receipt.session_id,
            turn_id: receipt.turn_id,
            accepted_seq: receipt.accepted_seq,
        }
    }
}

impl From<MessageReceipt> for WireResponse {
    fn from(receipt: MessageReceipt) -> Self {
        Self::MessageReceipt {
            session_id: receipt.session_id,
            message_id: receipt.message_id,
            accepted_control_seq: receipt.accepted_control_seq,
            observed_fact_seq: receipt.observed_fact_seq,
            state: receipt.state.into(),
        }
    }
}

impl From<MessageState> for WireMessageState {
    fn from(state: MessageState) -> Self {
        match state {
            MessageState::Pending => Self::Pending,
            MessageState::Claimed {
                activation_id,
                turn_id,
                step_id,
                entered_fact_seq,
            } => Self::Claimed {
                activation_id,
                turn_id,
                step_id,
                entered_fact_seq,
            },
            MessageState::Discarded {
                reason,
                control_seq,
            } => Self::Discarded {
                reason,
                control_seq,
            },
        }
    }
}

impl From<WireMessageState> for MessageState {
    fn from(state: WireMessageState) -> Self {
        match state {
            WireMessageState::Pending => Self::Pending,
            WireMessageState::Claimed {
                activation_id,
                turn_id,
                step_id,
                entered_fact_seq,
            } => Self::Claimed {
                activation_id,
                turn_id,
                step_id,
                entered_fact_seq,
            },
            WireMessageState::Discarded {
                reason,
                control_seq,
            } => Self::Discarded {
                reason,
                control_seq,
            },
        }
    }
}

pub(super) async fn get_handle(
    application: &Arc<dyn SessionApplication>,
    drafts: &UnpublishedDrafts,
    session_id: &SessionId,
) -> rsi_session::Result<Arc<dyn SessionHandle>> {
    let now = tokio::time::Instant::now();
    let mut drafts = drafts.lock().await;
    let expired = drafts
        .get(session_id)
        .is_some_and(|draft| draft.expires_at <= now)
        .then(|| drafts.remove(session_id))
        .flatten();
    let handle = drafts.get_mut(session_id).map(|draft| {
        draft.expires_at = now + UNPUBLISHED_DRAFT_IDLE_TIMEOUT;
        Arc::clone(&draft.handle)
    });
    drop(drafts);
    drop(expired);
    if let Some(handle) = handle {
        return Ok(handle);
    }
    application.attach(session_id).await
}

pub(super) fn unpublished_draft_deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + UNPUBLISHED_DRAFT_IDLE_TIMEOUT
}

pub(super) async fn prune_expired_drafts(drafts: &UnpublishedDrafts) {
    let mut drafts = drafts.lock().await;
    let expired = take_expired_drafts(&mut drafts, tokio::time::Instant::now());
    drop(drafts);
    drop(expired);
}

pub(super) fn take_expired_drafts(
    drafts: &mut BTreeMap<SessionId, UnpublishedDraft>,
    now: tokio::time::Instant,
) -> Vec<UnpublishedDraft> {
    let expired = drafts
        .iter()
        .filter(|(_, draft)| draft.expires_at <= now)
        .map(|(session_id, _)| session_id.clone())
        .collect::<Vec<_>>();
    expired
        .into_iter()
        .filter_map(|session_id| drafts.remove(&session_id))
        .collect()
}

pub(super) async fn serve_subscription(
    stream: &mut UnixStream,
    request_id: &str,
    session_id: &SessionId,
    cursor: ObservationCursor,
    application: Arc<dyn SessionApplication>,
    drafts: Arc<UnpublishedDrafts>,
    shutdown: CancellationToken,
) -> Result<(), SessionHostError> {
    let handle = match get_handle(&application, &drafts, session_id).await {
        Ok(handle) => handle,
        Err(error) => {
            return write_frame(
                stream,
                &ServerFrame::Response {
                    request_id: request_id.into(),
                    response: None,
                    error: Some(error.into()),
                },
            )
            .await;
        }
    };
    let mut observation = match handle.observe(cursor).await {
        Ok(observation) => observation,
        Err(error) => {
            return write_frame(
                stream,
                &ServerFrame::Response {
                    request_id: request_id.into(),
                    response: None,
                    error: Some(error.into()),
                },
            )
            .await;
        }
    };
    write_frame(
        stream,
        &ServerFrame::Response {
            request_id: request_id.into(),
            response: Some(WireResponse::Subscribed),
            error: None,
        },
    )
    .await?;
    loop {
        let next = tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                return write_frame(stream, &ServerFrame::End {
                    request_id: request_id.into(),
                    error: Some(WireError::ShuttingDown),
                }).await;
            }
            next = observation.next() => next,
        };
        match next {
            Some(Ok(update)) => {
                write_frame(
                    stream,
                    &ServerFrame::Event {
                        request_id: request_id.into(),
                        session_id: session_id.clone(),
                        update: update.into(),
                    },
                )
                .await?;
            }
            Some(Err(error)) => {
                return write_frame(
                    stream,
                    &ServerFrame::End {
                        request_id: request_id.into(),
                        error: Some(WireError::Backend {
                            message: error.to_string(),
                        }),
                    },
                )
                .await;
            }
            None => {
                return write_frame(
                    stream,
                    &ServerFrame::End {
                        request_id: request_id.into(),
                        error: None,
                    },
                )
                .await;
            }
        }
    }
}

pub(super) fn validate_wire_operation(operation: &WireOperation) -> Result<(), SessionHostError> {
    match operation {
        WireOperation::SubmitInput { content, .. }
            if content.is_empty() || content.len() > MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS =>
        {
            Err(SessionHostError::Invalid(format!(
                "message must contain 1..={MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS} content blocks"
            )))
        }
        WireOperation::Cancel {
            reason: Some(reason),
            ..
        } if reason.len() > MAXIMUM_AGENT_DIAGNOSTIC_BYTES => Err(SessionHostError::Invalid(
            format!("cancellation reason exceeds {MAXIMUM_AGENT_DIAGNOSTIC_BYTES} bytes"),
        )),
        WireOperation::AnswerApproval { approval_id, .. }
            if approval_id.is_empty() || approval_id.len() > MAXIMUM_APPROVAL_FIELD_BYTES =>
        {
            Err(SessionHostError::Invalid(format!(
                "approval id must be within 1..={MAXIMUM_APPROVAL_FIELD_BYTES} bytes"
            )))
        }
        _ => Ok(()),
    }
}

pub(super) const fn peer_belongs_to_effective_user(peer_uid: u32, expected_uid: u32) -> bool {
    peer_uid == expected_uid
}
