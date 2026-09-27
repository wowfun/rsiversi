//! Human-selected frozen references reuse the finite Session read and detail lanes.
use super::*;
use rsi_agent_session_protocol::{FrozenReference, ReferenceTextPage};

impl State {
    pub(super) fn refresh_references(&mut self) {
        self.reference_bytes = self
            .references
            .iter()
            .map(|reference| {
                serde_json::to_vec(reference)
                    .expect("validated reference serialization")
                    .len()
            })
            .sum();
        self.reference_status = if self.references.is_empty() {
            String::new()
        } else {
            format!(
                "{} frozen references · Actions → Draft references",
                self.references.len()
            )
        };
    }
    pub(super) fn reference_input(&self) -> Vec<MessageInput> {
        let mut content = Vec::new();
        if !self.editor.text().is_empty() {
            content.push(MessageInput::Text {
                text: self.editor.text().into(),
            });
        }
        content.extend(
            self.references
                .iter()
                .cloned()
                .map(|reference| MessageInput::Reference { reference }),
        );
        content
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consume the completed page at its UI handoff"
    )]
    pub(super) fn show_reference(&mut self, page: ReferenceTextPage) {
        let reference = &page.reference;
        let metadata = &reference.metadata;
        self.open_detail(format!("Reference {}\nCaptured through record {} · retained records {}..{} · {} bytes\nOmissions: {}\n\n{}",
            metadata.source, metadata.through_seq(), metadata.retained_interval().0,
            metadata.retained_interval().1, metadata.text_bytes,
            if metadata.omissions().is_empty() {"none".into()} else {format!("{:?}",metadata.omissions())},
            super::super::terminal_text(&page.text)));
        self.detail_previous = (page.offset > 0)
            .then(|| Action::PreviewReference(reference.clone(), page.offset.saturating_sub(8192)));
        self.detail_next = page
            .has_more
            .then(|| Action::PreviewReference(reference.clone(), page.next_offset));
        let items = if self
            .references
            .iter()
            .any(|item| item.snapshot == reference.snapshot)
        {
            vec![(
                "Remove from draft".into(),
                Action::RemoveReference(reference.snapshot.sha256.clone()),
            )]
        } else if self.references.len() < 4 {
            vec![(
                "Add frozen reference to draft".into(),
                Action::AddReference(reference.clone()),
            )]
        } else {
            Vec::new()
        };
        self.detail_actions = Some(Menu {
            title: "Reference actions".into(),
            items,
            selected: 0,
        });
        self.info("←/→ pages · ↑/↓ scroll · Enter actions · Esc returns to draft");
    }
}
impl Client {
    /// Other editors plus every frozen descriptor share the existing 4 MiB draft pool.
    pub(super) fn draft_reference_retention(&self) -> usize {
        self.state.reference_bytes
            + self
                .drafts
                .values()
                .map(|saved| saved.editor.retained_bytes() + saved.reference_bytes)
                .sum::<usize>()
    }
    #[expect(
        clippy::too_many_lines,
        reason = "One exhaustive projection keeps related state transitions and ownership visible together"
    )]
    pub(super) fn reference_action(&mut self, action: Action) {
        match action {
            Action::References => {
                let mut items: Vec<_> = self
                    .state
                    .references
                    .iter()
                    .map(|reference| {
                        (
                            format!(
                                "{} · captured through record {}{}",
                                reference.metadata.source,
                                reference.metadata.through_seq(),
                                if reference.metadata.omissions().is_empty() {
                                    ""
                                } else {
                                    " · shortened"
                                }
                            ),
                            Action::PreviewReference(reference.clone(), 0),
                        )
                    })
                    .collect();
                if self.state.references.len() < 4 {
                    items.push((
                        "Capture from recent session…".into(),
                        Action::ReferenceSources(None),
                    ));
                }
                self.state.menu = Some(Menu {
                    title: "Draft references".into(),
                    items,
                    selected: 0,
                });
            }
            Action::ReferenceSources(cursor) => {
                let application = self.application.clone();
                self.spawn_detail(async move {
                    let page = read(|| application.list_recent(cursor.as_ref(), 16)).await?;
                    let next = page
                        .sessions
                        .last()
                        .map(rsi_session_protocol::SessionSummary::cursor);
                    let mut items: Vec<_> = page
                        .sessions
                        .into_iter()
                        .map(|session| {
                            (
                                format!(
                                    "{} · {}",
                                    session.header.session_id(),
                                    session.header.canonical_cwd()
                                ),
                                Action::CaptureReference(session.header.session_id().clone()),
                            )
                        })
                        .collect();
                    if page.has_more {
                        items.push(("More sessions…".into(), Action::ReferenceSources(next)));
                    }
                    Ok(Update::Menu(Menu {
                        title: "Capture reference · select source".into(),
                        items,
                        selected: 0,
                    }))
                });
            }
            Action::CaptureReference(source) => {
                let handle = self.handle.clone();
                self.state.open_detail("Capturing conversation…".into());
                self.spawn_detail(async move {
                    let reference = handle.capture_reference(source).await.map_err(error)?;
                    handle
                        .preview_reference(reference, 0, 8192)
                        .await
                        .map(Update::Reference)
                        .map_err(error)
                });
            }
            Action::PreviewReference(reference, offset) => {
                let handle = self.handle.clone();
                self.state.open_detail("Reading frozen reference…".into());
                self.spawn_detail(async move {
                    handle
                        .preview_reference(reference, offset, 8192)
                        .await
                        .map(Update::Reference)
                        .map_err(error)
                });
            }
            Action::AddReference(reference) => {
                if let Err(problem) = self.add_reference(reference) {
                    self.state.notice(problem.to_string());
                } else {
                    self.state.escape();
                    self.state.info("Frozen reference added; draft retained");
                }
            }
            Action::RemoveReference(digest) => {
                self.state
                    .references
                    .retain(|reference| reference.snapshot.sha256 != digest);
                self.state.refresh_references();
                self.state.escape();
                self.state.info("Reference removed from draft");
            }
            _ => unreachable!("reference actions dispatched by the exhaustive caller"),
        }
    }
    fn add_reference(&mut self, reference: FrozenReference) -> Result<()> {
        reference.validate().map_err(error)?;
        if reference.metadata.target.session_id != *self.state.header.session_id()
            || reference.metadata.target.header_sha256
                != self.state.header.fingerprint().map_err(error)?
        {
            return Err(error("Reference belongs to a different target session"));
        }
        if self
            .state
            .references
            .iter()
            .any(|item| item.snapshot == reference.snapshot)
        {
            return Err(error("Reference is already in this draft"));
        }
        let mut content = self.state.reference_input();
        content.push(MessageInput::Reference {
            reference: reference.clone(),
        });
        rsi_session_protocol::validate_session_input(&content).map_err(error)?;
        let additional = serde_json::to_vec(&reference).map_err(error)?.len();
        if self.draft_reference_retention() + self.state.editor.retained_bytes() + additional
            > 4 * 1024 * 1024
        {
            return Err(error(
                "Draft retention is full; remove content before adding a reference",
            ));
        }
        self.state.references.push(reference);
        self.state.refresh_references();
        Ok(())
    }
}
