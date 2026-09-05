use super::*;

/// Remote adapter for the same product-level Session interface.
#[derive(Debug)]
pub struct UdsSessionApplication {
    socket: PathBuf,
    launch_key: String,
    host_epoch: HostEpoch,
    next_request: Arc<AtomicU64>,
    frame_budget: FrameReadBudget,
}

impl UdsSessionApplication {
    /// Probes and validates one exact daemon generation before returning an adapter.
    pub async fn connect(
        socket: impl Into<PathBuf>,
        launch_key: impl Into<String>,
        host_epoch: HostEpoch,
    ) -> rsi_session::Result<Self> {
        let application = Self {
            socket: socket.into(),
            launch_key: launch_key.into(),
            host_epoch,
            next_request: Arc::new(AtomicU64::new(1)),
            frame_budget: FrameReadBudget::default(),
        };
        validate_launch_key(&application.launch_key).map_err(host_as_session_error)?;
        let mut stream = application.connect_stream().await?;
        let response = tokio::time::timeout(
            PROBE_TIMEOUT,
            application.call_on_stream(&mut stream, WireOperation::Probe),
        )
        .await
        .map_err(|_| {
            SessionApplicationError::Backend("Session Host readiness probe timed out".into())
        })??;
        if !matches!(response, WireResponse::Ready) {
            return Err(SessionApplicationError::Backend(
                "Session Host returned the wrong readiness response".into(),
            ));
        }
        Ok(application)
    }

    async fn connect_stream(&self) -> rsi_session::Result<UnixStream> {
        let expected_product_build = session_host_product_build().map_err(host_as_session_error)?;
        let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(&self.socket))
            .await
            .map_err(|_| SessionApplicationError::Backend("Session Host connect timed out".into()))?
            .map_err(io_as_session_error)?;
        write_frame(
            &mut stream,
            &ClientFrame::Hello {
                protocol_epoch: SESSION_HOST_PROTOCOL_EPOCH,
                product_build: expected_product_build.into(),
                launch_key: self.launch_key.clone(),
                host_epoch: self.host_epoch.clone(),
            },
        )
        .await
        .map_err(host_as_session_error)?;
        match read_frame_with_timeout::<_, ServerFrame>(
            &mut stream,
            MAXIMUM_HANDSHAKE_FRAME_BYTES,
            &self.frame_budget,
            HANDSHAKE_READ_TIMEOUT,
            "server handshake",
        )
        .await
        .map_err(host_as_session_error)?
        {
            ServerFrame::HelloOk {
                protocol_epoch,
                product_build,
                launch_key,
                host_epoch,
            } if protocol_epoch == SESSION_HOST_PROTOCOL_EPOCH
                && product_build == expected_product_build
                && launch_key == self.launch_key
                && host_epoch == self.host_epoch =>
            {
                Ok(stream)
            }
            ServerFrame::HelloRejected { reason } => Err(SessionApplicationError::Backend(
                format!("Session Host handshake rejected: {reason}"),
            )),
            _ => Err(SessionApplicationError::Backend(
                "Session Host returned an incompatible handshake".into(),
            )),
        }
    }

    fn request_id(&self) -> String {
        format!(
            "request-{}",
            self.next_request.fetch_add(1, Ordering::Relaxed)
        )
    }

    async fn call(&self, operation: WireOperation) -> rsi_session::Result<WireResponse> {
        let mut stream = self.connect_stream().await?;
        self.call_on_stream(&mut stream, operation).await
    }

    async fn call_message(
        &self,
        operation: WireOperation,
        uploads: Vec<(u16, Arc<[u8]>)>,
        session_id: &SessionId,
        message_id: &MessageId,
    ) -> rsi_session::Result<WireResponse> {
        let mut stream = self.connect_stream().await?;
        let request_id = self.request_id();
        write_frame(
            &mut stream,
            &ClientFrame::Request {
                request_id: request_id.clone(),
                operation,
            },
        )
        .await
        .map_err(host_as_session_error)?;
        let has_uploads = !uploads.is_empty();
        for (upload_id, bytes) in uploads {
            for (index, chunk) in bytes.chunks(MAXIMUM_UPLOAD_CHUNK_BYTES).enumerate() {
                write_frame(
                    &mut stream,
                    &ClientFrame::UploadChunk {
                        request_id: request_id.clone(),
                        upload_id,
                        index: u32::try_from(index).map_err(|_| {
                            SessionApplicationError::Invalid(
                                "image upload chunk count is unsupported".into(),
                            )
                        })?,
                        data: base64::engine::general_purpose::STANDARD.encode(chunk),
                    },
                )
                .await
                .map_err(host_as_session_error)?;
            }
        }
        if has_uploads {
            write_frame(
                &mut stream,
                &ClientFrame::UploadEnd {
                    request_id: request_id.clone(),
                },
            )
            .await
            .map_err(host_as_session_error)?;
        }
        let response = read_frame_with_timeout::<_, ServerFrame>(
            &mut stream,
            MAXIMUM_FRAME_BYTES,
            &self.frame_budget,
            RESPONSE_READ_TIMEOUT,
            "server response",
        )
        .await
        .map_err(|_| message_outcome_unknown(session_id, message_id))?;
        match response {
            ServerFrame::Response {
                request_id: response_id,
                response,
                error,
            } if response_id == request_id => {
                decode_message_response(response, error, session_id, message_id)
            }
            _ => Err(message_outcome_unknown(session_id, message_id)),
        }
    }

    async fn call_on_stream(
        &self,
        stream: &mut UnixStream,
        operation: WireOperation,
    ) -> rsi_session::Result<WireResponse> {
        let request_id = self.request_id();
        write_frame(
            stream,
            &ClientFrame::Request {
                request_id: request_id.clone(),
                operation,
            },
        )
        .await
        .map_err(host_as_session_error)?;
        match read_frame_with_timeout::<_, ServerFrame>(
            stream,
            MAXIMUM_FRAME_BYTES,
            &self.frame_budget,
            RESPONSE_READ_TIMEOUT,
            "server response",
        )
        .await
        .map_err(host_as_session_error)?
        {
            ServerFrame::Response {
                request_id: response_id,
                response,
                error,
            } if response_id == request_id => decode_response(response, error),
            _ => Err(SessionApplicationError::Backend(
                "Session Host response did not match its request".into(),
            )),
        }
    }

    async fn sequence(
        &self,
        operation: WireOperation,
    ) -> rsi_session::Result<(WireResponse, Vec<WireItem>)> {
        let contract = SequenceContract::from_operation(&operation)?;
        let mut stream = self.connect_stream().await?;
        let request_id = self.request_id();
        write_frame(
            &mut stream,
            &ClientFrame::Request {
                request_id: request_id.clone(),
                operation,
            },
        )
        .await
        .map_err(host_as_session_error)?;
        let response = match read_frame_with_timeout::<_, ServerFrame>(
            &mut stream,
            MAXIMUM_FRAME_BYTES,
            &self.frame_budget,
            RESPONSE_READ_TIMEOUT,
            "server sequence response",
        )
        .await
        .map_err(host_as_session_error)?
        {
            ServerFrame::Response {
                request_id: response_id,
                response,
                error,
            } if response_id == request_id => decode_response(response, error)?,
            _ => {
                return Err(SessionApplicationError::Backend(
                    "Session Host sequence response did not match its request".into(),
                ));
            }
        };
        contract.validate_start(&response)?;
        let mut items = Vec::new();
        let mut retained_bytes = 0_usize;
        loop {
            match read_frame_with_timeout::<_, ServerFrame>(
                &mut stream,
                MAXIMUM_FRAME_BYTES,
                &self.frame_budget,
                RESPONSE_READ_TIMEOUT,
                "server sequence item",
            )
            .await
            .map_err(host_as_session_error)?
            {
                ServerFrame::Item {
                    request_id: response_id,
                    item,
                } if response_id == request_id => {
                    retained_bytes = contract.admit_item(items.len(), retained_bytes, &item)?;
                    items.push(*item);
                }
                ServerFrame::End {
                    request_id: response_id,
                    error: None,
                } if response_id == request_id => return Ok((response, items)),
                ServerFrame::End {
                    request_id: response_id,
                    error: Some(error),
                } if response_id == request_id => return Err(error.into()),
                _ => {
                    return Err(SessionApplicationError::Backend(
                        "Session Host sequence item did not match its request".into(),
                    ));
                }
            }
        }
    }
}

#[derive(Debug)]
pub(super) enum SequenceContract {
    Recent { limit: usize },
    History { session_id: SessionId, limit: usize },
    PendingApprovals,
}

impl SequenceContract {
    pub(super) fn from_operation(operation: &WireOperation) -> rsi_session::Result<Self> {
        match operation {
            WireOperation::ListRecent { limit, .. } => Ok(Self::Recent { limit: *limit }),
            WireOperation::History {
                session_id, limit, ..
            } => Ok(Self::History {
                session_id: session_id.clone(),
                limit: *limit,
            }),
            WireOperation::PendingApprovals { .. } => Ok(Self::PendingApprovals),
            _ => Err(SessionApplicationError::Backend(
                "Session Host sequence contract does not match its operation".into(),
            )),
        }
    }

    pub(super) fn validate_start(&self, response: &WireResponse) -> rsi_session::Result<()> {
        if matches!(
            (self, response),
            (Self::Recent { .. }, WireResponse::RecentStart { .. })
                | (Self::History { .. }, WireResponse::HistoryStart { .. })
                | (Self::PendingApprovals, WireResponse::PendingApprovalsStart)
        ) {
            Ok(())
        } else {
            Err(unexpected_response())
        }
    }

    pub(super) fn admit_item(
        &self,
        current_items: usize,
        current_bytes: usize,
        item: &WireItem,
    ) -> rsi_session::Result<usize> {
        admit_sequence_item(current_items)?;
        let (maximum_items, item_bytes) = match (self, item) {
            (Self::Recent { limit }, WireItem::Session { header }) => (
                *limit,
                serde_json::to_vec(header)
                    .map_err(|error| SessionApplicationError::Backend(error.to_string()))?
                    .len(),
            ),
            (
                Self::History { session_id, limit },
                WireItem::Fact {
                    session_id: item_session_id,
                    fact,
                },
            ) if item_session_id == session_id => (*limit, fact.encoded_len()),
            (Self::PendingApprovals, WireItem::Approval { request }) => (
                MAXIMUM_SEQUENCE_ITEMS,
                serde_json::to_vec(request)
                    .map_err(|error| SessionApplicationError::Backend(error.to_string()))?
                    .len(),
            ),
            _ => {
                return Err(SessionApplicationError::Backend(
                    "Session Host sequence item violates its operation contract".into(),
                ));
            }
        };
        if current_items >= maximum_items {
            return Err(SessionApplicationError::Backend(
                "Session Host sequence exceeds its operation count bound".into(),
            ));
        }
        let next_bytes = current_bytes.checked_add(item_bytes).ok_or_else(|| {
            SessionApplicationError::Backend("Session Host sequence byte count overflowed".into())
        })?;
        let maximum_bytes = match self {
            Self::Recent { limit } => limit
                .checked_mul(rsi_agent_session_protocol::MAXIMUM_SESSION_HEADER_BYTES)
                .ok_or_else(|| {
                    SessionApplicationError::Backend(
                        "Session Host recent sequence byte bound overflowed".into(),
                    )
                })?,
            Self::History { .. } | Self::PendingApprovals => MAXIMUM_STORE_FACT_PAGE_BYTES,
        };
        if next_bytes > maximum_bytes {
            return Err(SessionApplicationError::Backend(
                "Session Host sequence exceeds its aggregate byte bound".into(),
            ));
        }
        Ok(next_bytes)
    }
}

pub(super) fn admit_sequence_item(current_items: usize) -> rsi_session::Result<()> {
    if current_items >= MAXIMUM_SEQUENCE_ITEMS {
        return Err(SessionApplicationError::Backend(format!(
            "Session Host sequence exceeds its {MAXIMUM_SEQUENCE_ITEMS}-item bound"
        )));
    }
    Ok(())
}

#[async_trait]
impl SessionApplication for UdsSessionApplication {
    async fn create(&self, request: CreateSession) -> rsi_session::Result<Arc<dyn SessionHandle>> {
        let expected_session_id = request.session_id.clone();
        let cwd = canonical_workspace_directory(&request.cwd).await?;
        let cwd = cwd.to_str().ok_or_else(|| {
            SessionApplicationError::Invalid("workspace path is not UTF-8".into())
        })?;
        let response = self
            .call(WireOperation::Create {
                cwd: cwd.into(),
                session_id: request.session_id,
                agent_preset_id: request.agent_preset_id,
                workspace_trust: request.workspace_trust,
            })
            .await?;
        self.handle_response(response, expected_session_id.as_ref())
    }

    async fn attach(&self, session_id: &SessionId) -> rsi_session::Result<Arc<dyn SessionHandle>> {
        let response = self
            .call(WireOperation::Attach {
                session_id: session_id.clone(),
            })
            .await?;
        self.handle_response(response, Some(session_id))
    }

    async fn list_recent(
        &self,
        after: Option<&RecentSessionCursor>,
        limit: usize,
    ) -> rsi_session::Result<RecentSessionPage> {
        let (response, items) = self
            .sequence(WireOperation::ListRecent {
                after: after.map(|cursor| WireRecentCursor {
                    created_at_ms: cursor.created_at_ms,
                    session_id: cursor.session_id.clone(),
                }),
                limit,
            })
            .await?;
        match response {
            WireResponse::RecentStart { has_more } => {
                let page = RecentSessionPage {
                    sessions: items
                        .into_iter()
                        .map(|item| match item {
                            WireItem::Session { header } => Ok(SessionSummary { header }),
                            _ => Err(unexpected_response()),
                        })
                        .collect::<rsi_session::Result<Vec<_>>>()?,
                    has_more,
                };
                validate_remote_recent_page(after, &page)?;
                Ok(page)
            }
            _ => Err(unexpected_response()),
        }
    }
}

impl UdsSessionApplication {
    fn handle_response(
        &self,
        response: WireResponse,
        expected_session_id: Option<&SessionId>,
    ) -> rsi_session::Result<Arc<dyn SessionHandle>> {
        match response {
            WireResponse::Session { header }
                if expected_session_id.is_none_or(|expected| expected == header.session_id()) =>
            {
                Ok(Arc::new(UdsSessionHandle {
                    application: Arc::new(self.clone_inner()),
                    header: *header,
                }))
            }
            WireResponse::Session { .. } => Err(SessionApplicationError::Backend(
                "Session Host returned a Header for a different Session identity".into(),
            )),
            _ => Err(unexpected_response()),
        }
    }

    fn clone_inner(&self) -> Self {
        Self {
            socket: self.socket.clone(),
            launch_key: self.launch_key.clone(),
            host_epoch: self.host_epoch.clone(),
            next_request: Arc::clone(&self.next_request),
            frame_budget: self.frame_budget.clone(),
        }
    }
}

#[derive(Debug)]
pub(super) struct UdsSessionHandle {
    application: Arc<UdsSessionApplication>,
    header: SessionHeader,
}

#[async_trait]
impl SessionHandle for UdsSessionHandle {
    async fn header(&self) -> rsi_session::Result<SessionHeader> {
        Ok(self.header.clone())
    }

    async fn submit(&self, request: SubmitInput) -> rsi_session::Result<MessageReceipt> {
        validate_session_input(&request.content)?;
        let message_id = request.message_id;
        let mut uploads = Vec::new();
        let mut content = Vec::with_capacity(request.content.len());
        for block in request.content {
            match block {
                SessionInput::Text { text } => content.push(WireInputBlock::Text { text }),
                SessionInput::Image { bytes } => {
                    let upload_id = u16::try_from(uploads.len()).map_err(|_| {
                        SessionApplicationError::Invalid(
                            "Session input contains too many images".into(),
                        )
                    })?;
                    content.push(WireInputBlock::Image {
                        upload_id,
                        bytes: u64::try_from(bytes.len()).map_err(|_| {
                            SessionApplicationError::Invalid(
                                "Session input image length is unsupported".into(),
                            )
                        })?,
                        sha256: hex::encode(sha2::Sha256::digest(bytes.as_ref())),
                    });
                    uploads.push((upload_id, bytes));
                }
            }
        }
        let operation = WireOperation::SubmitInput {
            session_id: self.header.session_id().clone(),
            message_id: message_id.clone(),
            content,
            model: request.model,
            sandbox: request.sandbox,
        };
        let response = self
            .application
            .call_message(operation, uploads, self.header.session_id(), &message_id)
            .await?;
        decode_message_receipt(response, self.header.session_id(), &message_id)
    }

    async fn message_status(&self, message_id: &MessageId) -> rsi_session::Result<MessageReceipt> {
        decode_message_receipt(
            self.application
                .call(WireOperation::MessageStatus {
                    session_id: self.header.session_id().clone(),
                    message_id: message_id.clone(),
                })
                .await?,
            self.header.session_id(),
            message_id,
        )
    }

    async fn generate_image(&self, request: SubmitDirectImage) -> rsi_session::Result<TurnReceipt> {
        let turn_id = request.turn_id;
        decode_receipt(
            self.application
                .call(WireOperation::SubmitImage {
                    session_id: self.header.session_id().clone(),
                    turn_id: turn_id.clone(),
                    model: request.model,
                    request: request.request,
                })
                .await?,
            self.header.session_id(),
            &turn_id,
        )
    }

    async fn cancel(
        &self,
        target: CancelTarget,
        reason: Option<String>,
    ) -> rsi_session::Result<CancelResult> {
        let target = match target {
            CancelTarget::Message(message_id) => WireCancelTarget::Message { message_id },
            CancelTarget::Turn(turn_id) => WireCancelTarget::Turn { turn_id },
        };
        match self
            .application
            .call(WireOperation::Cancel {
                session_id: self.header.session_id().clone(),
                target,
                reason,
            })
            .await?
        {
            WireResponse::Cancel {
                accepted,
                already_terminal,
            } => Ok(CancelResult {
                accepted,
                already_terminal,
            }),
            _ => Err(unexpected_response()),
        }
    }

    async fn history_before(
        &self,
        exclusive_before_seq: Option<u64>,
        limit: usize,
    ) -> rsi_session::Result<SessionHistoryPage> {
        let (response, items) = self
            .application
            .sequence(WireOperation::History {
                session_id: self.header.session_id().clone(),
                exclusive_before_seq,
                limit,
            })
            .await?;
        match response {
            WireResponse::HistoryStart {
                before_seq,
                durable_seq,
                has_more,
            } => {
                let page = StoreBackwardFactPage {
                    before_seq,
                    facts: items
                        .into_iter()
                        .map(|item| match item {
                            WireItem::Fact { fact, .. } => Ok(fact),
                            _ => Err(unexpected_response()),
                        })
                        .collect::<rsi_session::Result<Vec<_>>>()?,
                    durable_seq,
                    has_more,
                };
                page.validate().map_err(|error| {
                    SessionApplicationError::Backend(format!(
                        "Session Host history page violates its bound: {error}"
                    ))
                })?;
                Ok(SessionHistoryPage {
                    before_seq: page.before_seq,
                    facts: page.facts,
                    durable_seq: page.durable_seq,
                    has_more: page.has_more,
                })
            }
            _ => Err(unexpected_response()),
        }
    }

    async fn observe(
        &self,
        cursor: ObservationCursor,
    ) -> rsi_session::Result<SessionObservationStream> {
        let mut stream = self.application.connect_stream().await?;
        let request_id = self.application.request_id();
        write_frame(
            &mut stream,
            &ClientFrame::Request {
                request_id: request_id.clone(),
                operation: WireOperation::Subscribe {
                    session_id: self.header.session_id().clone(),
                    cursor: WireObservationCursor {
                        control_seq: cursor.control_seq,
                        fact_seq: cursor.fact_seq,
                    },
                },
            },
        )
        .await
        .map_err(host_as_session_error)?;
        match read_frame_with_timeout::<_, ServerFrame>(
            &mut stream,
            MAXIMUM_FRAME_BYTES,
            &self.application.frame_budget,
            RESPONSE_READ_TIMEOUT,
            "subscription response",
        )
        .await
        .map_err(host_as_session_error)?
        {
            ServerFrame::Response {
                request_id: response_id,
                response: Some(WireResponse::Subscribed),
                error: None,
            } if response_id == request_id => {}
            ServerFrame::Response {
                request_id: response_id,
                response: None,
                error: Some(error),
            } if response_id == request_id => return Err(error.into()),
            _ => return Err(unexpected_response()),
        }
        let frame_budget = self.application.frame_budget.clone();
        let expected_session_id = self.header.session_id().clone();
        let mut contract = ObservationStreamContract::new(cursor);
        Ok(Box::pin(async_stream::stream! {
            loop {
                match read_subscription_frame::<_, ServerFrame>(
                    &mut stream,
                    MAXIMUM_FRAME_BYTES,
                    &frame_budget,
                ).await {
                    Ok(frame) => match decode_subscription_frame(
                        frame,
                        &request_id,
                        &expected_session_id,
                    ) {
                        Ok(DecodedSubscriptionFrame::Update(update)) => {
                            if let Err(error) = contract.admit(&update) {
                                yield Err(error);
                                break;
                            }
                            yield Ok(update);
                        }
                        Ok(DecodedSubscriptionFrame::End(error)) => {
                            if let Some(error) = error {
                                yield Err(wire_as_turn_error(error));
                            }
                            break;
                        }
                        Err(error) => {
                            yield Err(error);
                            break;
                        }
                    },
                    Err(error) => {
                        yield Err(TurnError::Invariant(error.to_string()));
                        break;
                    }
                }
            }
        }))
    }

    async fn pending_approvals(&self) -> rsi_session::Result<Vec<ApprovalRequest>> {
        let (response, items) = self
            .application
            .sequence(WireOperation::PendingApprovals {
                session_id: self.header.session_id().clone(),
            })
            .await?;
        if !matches!(response, WireResponse::PendingApprovalsStart) {
            return Err(unexpected_response());
        }
        let root = self
            .header
            .fork_origin()
            .map_or(self.header.session_id(), |origin| &origin.root_session_id);
        let mut verified = std::collections::BTreeSet::from([self.header.session_id().clone()]);
        let mut requests = Vec::with_capacity(items.len());
        for item in items {
            let WireItem::Approval { request } = item else {
                return Err(unexpected_response());
            };
            let subject = SessionId::new(request.subject.session_id())
                .map_err(|error| SessionApplicationError::Backend(error.to_string()))?;
            if !verified.contains(&subject) {
                let header = self.application.attach(&subject).await?.header().await?;
                let subject_root = header
                    .fork_origin()
                    .map_or(header.session_id(), |origin| &origin.root_session_id);
                if subject_root != root {
                    return Err(SessionApplicationError::Backend(
                        "approval subject is outside the initiating Agent tree".into(),
                    ));
                }
                verified.insert(subject);
            }
            requests.push(request);
        }
        Ok(requests)
    }

    async fn answer_approval(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> rsi_session::Result<bool> {
        match self
            .application
            .call(WireOperation::AnswerApproval {
                session_id: self.header.session_id().clone(),
                approval_id: approval_id.into(),
                decision,
            })
            .await?
        {
            WireResponse::ApprovalAnswer { accepted } => Ok(accepted),
            _ => Err(unexpected_response()),
        }
    }
}

pub(super) fn decode_receipt(
    response: WireResponse,
    expected_session_id: &SessionId,
    expected_turn_id: &TurnId,
) -> rsi_session::Result<TurnReceipt> {
    match response {
        WireResponse::TurnReceipt {
            session_id,
            turn_id,
            accepted_seq,
        } if &session_id == expected_session_id && &turn_id == expected_turn_id => {
            Ok(TurnReceipt {
                session_id,
                turn_id,
                accepted_seq,
            })
        }
        WireResponse::TurnReceipt { .. } => Err(SessionApplicationError::Backend(
            "Session Host returned a receipt for a different Session or Turn identity".into(),
        )),
        _ => Err(unexpected_response()),
    }
}

pub(super) fn decode_message_receipt(
    response: WireResponse,
    expected_session_id: &SessionId,
    expected_message_id: &MessageId,
) -> rsi_session::Result<MessageReceipt> {
    match response {
        WireResponse::MessageReceipt {
            session_id,
            message_id,
            accepted_control_seq,
            observed_fact_seq,
            state,
        } if &session_id == expected_session_id && &message_id == expected_message_id => {
            let receipt = MessageReceipt {
                session_id,
                message_id,
                accepted_control_seq,
                observed_fact_seq,
                state: state.into(),
            };
            receipt.validate().map_err(|error| {
                SessionApplicationError::Backend(format!(
                    "Session Host message receipt violates its sequence contract: {error}"
                ))
            })?;
            Ok(receipt)
        }
        WireResponse::MessageReceipt { .. } => Err(SessionApplicationError::Backend(
            "Session Host returned a receipt for a different Session or Message identity".into(),
        )),
        _ => Err(unexpected_response()),
    }
}

pub(super) fn validate_remote_recent_page(
    after: Option<&RecentSessionCursor>,
    page: &RecentSessionPage,
) -> rsi_session::Result<()> {
    StoreRecentSessionPage {
        after: after.map(|cursor| StoreRecentSessionCursor {
            created_at_ms: cursor.created_at_ms,
            session_id: cursor.session_id.clone(),
        }),
        sessions: page
            .sessions
            .iter()
            .map(|summary| StoreRecentSession {
                header: summary.header.clone(),
            })
            .collect(),
        has_more: page.has_more,
    }
    .validate()
    .map_err(|error| {
        SessionApplicationError::Backend(format!(
            "Session Host recent-session page violates its ordering contract: {error}"
        ))
    })
}

pub(super) fn decode_response(
    response: Option<WireResponse>,
    error: Option<WireError>,
) -> rsi_session::Result<WireResponse> {
    match (response, error) {
        (Some(response), None) => Ok(response),
        (None, Some(error)) => Err(error.into()),
        _ => Err(SessionApplicationError::Backend(
            "Session Host returned an invalid response envelope".into(),
        )),
    }
}

pub(super) fn decode_message_response(
    response: Option<WireResponse>,
    error: Option<WireError>,
    session_id: &SessionId,
    message_id: &MessageId,
) -> rsi_session::Result<WireResponse> {
    match (response, error) {
        (Some(response), None) => Ok(response),
        (None, Some(error)) => Err(error.into()),
        _ => Err(message_outcome_unknown(session_id, message_id)),
    }
}

pub(super) fn unexpected_response() -> SessionApplicationError {
    SessionApplicationError::Backend("Session Host returned the wrong response variant".into())
}

pub(super) fn wire_as_turn_error(error: WireError) -> TurnError {
    match error {
        WireError::ShuttingDown => TurnError::ShuttingDown,
        other => TurnError::Invariant(SessionApplicationError::from(other).to_string()),
    }
}

pub(super) enum DecodedSubscriptionFrame {
    Update(SessionObservation),
    End(Option<WireError>),
}

#[derive(Debug)]
#[allow(clippy::struct_field_names)] // Keep observed cursors distinct from their durable watermarks at the wire boundary.
pub(super) struct ObservationStreamContract {
    control_seq: u64,
    fact_seq: u64,
    durable_control_seq: u64,
    durable_fact_seq: u64,
}

impl ObservationStreamContract {
    pub(super) const fn new(cursor: ObservationCursor) -> Self {
        Self {
            control_seq: cursor.control_seq,
            fact_seq: cursor.fact_seq,
            durable_control_seq: cursor.control_seq,
            durable_fact_seq: cursor.fact_seq,
        }
    }

    pub(super) fn admit(
        &mut self,
        update: &SessionObservation,
    ) -> rsi_agent_turn_protocol::Result<()> {
        match update {
            SessionObservation::Control {
                record,
                durable_control_seq,
            } => {
                let expected = self.control_seq.checked_add(1).ok_or_else(|| {
                    TurnError::Invariant("subscription control cursor exhausted".into())
                })?;
                if record.seq() != expected
                    || *durable_control_seq < record.seq()
                    || *durable_control_seq < self.durable_control_seq
                {
                    return Err(TurnError::Invariant(
                        "Session Host subscription control sequence is discontinuous or regressing"
                            .into(),
                    ));
                }
                self.control_seq = record.seq();
                self.durable_control_seq = *durable_control_seq;
            }
            SessionObservation::Fact {
                fact,
                durable_fact_seq,
            } => {
                let expected = self.fact_seq.checked_add(1).ok_or_else(|| {
                    TurnError::Invariant("subscription Fact cursor exhausted".into())
                })?;
                if fact.seq() != expected
                    || *durable_fact_seq < fact.seq()
                    || *durable_fact_seq < self.durable_fact_seq
                {
                    return Err(TurnError::Invariant(
                        "Session Host subscription Fact sequence is discontinuous or regressing"
                            .into(),
                    ));
                }
                self.fact_seq = fact.seq();
                self.durable_fact_seq = *durable_fact_seq;
            }
        }
        Ok(())
    }
}

pub(super) fn decode_subscription_frame(
    frame: ServerFrame,
    expected_request_id: &str,
    expected_session_id: &SessionId,
) -> Result<DecodedSubscriptionFrame, TurnError> {
    match frame {
        ServerFrame::Event {
            request_id,
            session_id,
            update,
        } if request_id == expected_request_id && &session_id == expected_session_id => {
            Ok(DecodedSubscriptionFrame::Update(update.into()))
        }
        ServerFrame::End { request_id, error } if request_id == expected_request_id => {
            Ok(DecodedSubscriptionFrame::End(error))
        }
        _ => Err(TurnError::Invariant(
            "Session Host stream response did not match its request".into(),
        )),
    }
}
