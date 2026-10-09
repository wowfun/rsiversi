use super::{Action, Client, Menu, Update, error};
use rsi_history_api::{ConversationIdentity, Coverage, QueryScope, Reply, Request, Scope};
use std::fmt::Write as _;
fn coverage(value: &Coverage) -> String {
    format!(
        "Indexed through {} · observed through {} · omitted {}\n{}",
        value.indexed_through,
        value.observed_through,
        value.omissions,
        if value.has_more {
            "More indexing needed"
        } else {
            "Caught up at this observation"
        }
    )
}
impl Client {
    pub(super) fn search_all_history(&mut self, query: String, workspace_only: bool) {
        let scope = if workspace_only {
            QueryScope::Workspace {
                workspace: rsi_workspace_protocol::WorkspaceId::from_coordinates(
                    self.state.header.coordinates(),
                ),
            }
        } else {
            QueryScope::AccessibleHost
        };
        let request = Request::Query {
            scope,
            query,
            after: None,
        };
        self.history_query = Some(request.clone());
        let Some(client) = self.text_history.clone() else {
            self.state.notice("History text search is unavailable");
            return;
        };
        self.state.open_detail("Discovering saved history…".into());
        self.spawn_detail(async move {
            let Request::Query { scope, .. } = &request else {
                unreachable!()
            };
            client
                .call(Request::Discover {
                    scope: scope.clone(),
                    after: None,
                })
                .await
                .map_err(error)?;
            let reply = client.call(request.clone()).await.map_err(error)?;
            Ok(Update::HistorySearch(Box::new(request), Box::new(reply)))
        });
    }
    pub(super) fn search_history(&mut self, conversation: ConversationIdentity, query: String) {
        let workspace =
            rsi_workspace_protocol::WorkspaceId::from_coordinates(self.state.header.coordinates());
        let request = Request::Search {
            scope: Scope {
                workspace,
                conversation,
            },
            query,
            after: None,
        };
        self.history_query = Some(request.clone());
        self.history_request(request);
    }
    pub(super) fn history_request(&mut self, request: Request) {
        let Some(client) = self.text_history.clone() else {
            self.state.notice("History text search is unavailable");
            return;
        };
        self.state.open_detail("Reading history owner…".into());
        self.spawn_detail(async move {
            let reply = client.call(request.clone()).await.map_err(error)?;
            Ok(Update::HistorySearch(Box::new(request), Box::new(reply)))
        });
    }
    fn history_actions(&self, scope: &Scope) -> Vec<(String, Action)> {
        let mut items = vec![(
            "Index next batch".into(),
            Action::HistoryRequest(Box::new(Request::Advance {
                scope: scope.clone(),
            })),
        )];
        if let Some(query) = &self.history_query {
            items.push((
                "Search indexed text".into(),
                Action::HistoryRequest(Box::new(query.clone())),
            ));
        }
        items.push((
            "Rebuild this source's index".into(),
            Action::HistoryRequest(Box::new(Request::Rebuild {
                scope: scope.clone(),
            })),
        ));
        items
    }
    #[expect(
        clippy::too_many_lines,
        reason = "One exhaustive reply projection keeps read, selection and draft ownership together"
    )]
    pub(super) fn show_history(&mut self, request: &Request, reply: Reply) {
        if matches!(
            request,
            Request::Query { .. }
                | Request::Discover { .. }
                | Request::Progress { .. }
                | Request::Reset { .. }
        ) {
            self.show_history_query(request, reply);
            return;
        }
        let scope = request.scope().expect("exact source request").clone();
        match reply {
            Reply::Coverage { coverage: progress } => {
                self.state
                    .open_detail(format!("History indexing\n{}", coverage(&progress)));
                self.state.detail_actions = Some(Menu {
                    title: "History indexing actions".into(),
                    items: self.history_actions(&scope),
                    selected: 0,
                });
            }
            Reply::Hits {
                coverage: progress,
                hits,
                next,
            } => {
                let mut text = format!("History text search\n{}\n\n", coverage(&progress));
                let mut items = Vec::new();
                for (index, hit) in hits.into_iter().enumerate() {
                    let preview: String = hit.preview.chars().take(100).collect();
                    let label = format!(
                        "{} · {:?} · record {}",
                        index + 1,
                        hit.original.record.kind,
                        hit.original.record.sequence
                    );
                    let _ = write!(
                        text,
                        "{label}\n{}\n\n",
                        super::super::terminal_text(&preview)
                    );
                    items.push((
                        format!("Open original {label}"),
                        Action::HistoryRequest(Box::new(Request::Read {
                            scope: scope.clone(),
                            hit,
                            offset: 0,
                        })),
                    ));
                }
                if items.is_empty() {
                    text.push_str("No matches in the indexed portion.\n");
                }
                if let (Some(next), Request::Search { query, .. }) = (next, &request) {
                    items.push((
                        "More matches".into(),
                        Action::HistoryRequest(Box::new(Request::Search {
                            scope: scope.clone(),
                            query: query.clone(),
                            after: Some(next),
                        })),
                    ));
                }
                items.extend(self.history_actions(&scope));
                self.state.open_detail(text);
                self.state.detail_actions = Some(Menu {
                    title: "History matches and indexing".into(),
                    items,
                    selected: 0,
                });
            }
            Reply::Original {
                hit,
                offset,
                next_offset,
                has_more,
                text,
            } => {
                self.state.open_detail(format!(
                    "Verified original · record {}\nBytes {}–{} of {}\n\n{}",
                    hit.original.record.sequence,
                    offset,
                    next_offset,
                    hit.original.end,
                    super::super::terminal_text(&text)
                ));
                self.state.detail_previous = (offset > 0).then(|| {
                    Action::HistoryRequest(Box::new(Request::Read {
                        scope: scope.clone(),
                        hit: hit.clone(),
                        offset: offset.saturating_sub(65536),
                    }))
                });
                self.state.detail_next = has_more.then(|| {
                    Action::HistoryRequest(Box::new(Request::Read {
                        scope: scope.clone(),
                        hit: hit.clone(),
                        offset: next_offset,
                    }))
                });
                let select = |start, end| {
                    Action::HistoryRequest(Box::new(Request::Freeze {
                        scope: scope.clone(),
                        hit: hit.clone(),
                        target: self.state.header.session_id().clone(),
                        start,
                        end,
                    }))
                };
                let mut items = Vec::new();
                if offset < next_offset {
                    items.push((
                        "Freeze displayed original page".into(),
                        select(offset, next_offset),
                    ));
                }
                let mut start = offset;
                for (index, line) in text.split_inclusive('\n').enumerate() {
                    let visible = line.trim_end_matches(['\r', '\n']);
                    if !visible.trim().is_empty() {
                        let prefix: String = visible.chars().take(64).collect();
                        items.push((
                            format!(
                                "Freeze line {}: {}",
                                index + 1,
                                super::super::terminal_text(&prefix)
                            ),
                            select(start, start + visible.len()),
                        ));
                        if items.len() > 128 {
                            break;
                        }
                    }
                    start += line.len();
                }
                self.state.detail_actions = Some(Menu {
                    title: "Select original page or line (first 128)".into(),
                    items,
                    selected: 0,
                });
            }
            Reply::Frozen { reference } => {
                self.reference_action(Action::PreviewReference(reference, 0));
            }
            Reply::Matches { .. } | Reply::Progress { .. } | Reply::Stale { .. } => {
                unreachable!("validated exact reply")
            }
        }
        self.state
            .info("↑/↓ scroll · ←/→ original pages · Enter actions · Esc returns to draft");
    }
    #[expect(
        clippy::too_many_lines,
        reason = "One reply projection keeps finite history progress, source actions and continuation coherent."
    )]
    fn show_history_query(&mut self, request: &Request, reply: Reply) {
        let scope = match request {
            Request::Query { scope, .. }
            | Request::Discover { scope, .. }
            | Request::Progress { scope, .. }
            | Request::Reset { scope, .. } => scope.clone(),
            _ => unreachable!(),
        };
        let mut text = String::from("Saved history\n");
        let mut items = Vec::new();
        let progress = match reply {
            Reply::Matches {
                progress,
                matches,
                next,
            } => {
                for result in matches {
                    let label = format!("{} · {:?}", result.label, result.scope.conversation);
                    let _ = writeln!(
                        text,
                        "\n{}\n{}",
                        super::super::terminal_text(&label),
                        super::super::terminal_text(&result.hit.preview)
                    );
                    items.push((
                        format!("Open original · {}", super::super::terminal_text(&label)),
                        Action::HistoryRequest(Box::new(Request::Read {
                            scope: result.scope,
                            hit: result.hit,
                            offset: 0,
                        })),
                    ));
                }
                if let (Some(after), Request::Query { query, .. }) = (next, request) {
                    items.push((
                        "More matches".into(),
                        Action::HistoryRequest(Box::new(Request::Query {
                            scope: scope.clone(),
                            query: query.clone(),
                            after: Some(after),
                        })),
                    ));
                }
                Some(progress)
            }
            Reply::Progress {
                progress,
                sources,
                next,
            } => {
                for source in sources {
                    let _ = writeln!(
                        text,
                        "\n{} · {:?}\n{}{}",
                        super::super::terminal_text(&source.label),
                        source.scope.conversation,
                        coverage(&source.coverage),
                        if source.unavailable {
                            "\nUnavailable in this pass; refresh to retry"
                        } else {
                            ""
                        }
                    );
                    items.push((
                        format!("Rebuild {:?}", source.scope.conversation),
                        Action::HistoryRequest(Box::new(Request::Rebuild {
                            scope: source.scope,
                        })),
                    ));
                }
                if let Some(after) = next {
                    let resetting = matches!(request, Request::Reset { .. });
                    items.push((
                        if resetting {
                            "Continue workspace reset"
                        } else {
                            "More source coverage"
                        }
                        .into(),
                        Action::HistoryRequest(Box::new(if resetting {
                            Request::Reset {
                                scope: scope.clone(),
                                after: Some(after),
                            }
                        } else {
                            Request::Progress {
                                scope: scope.clone(),
                                after: Some(after),
                            }
                        })),
                    ));
                }
                Some(progress)
            }
            Reply::Stale { reason } => {
                text.push_str(&super::super::terminal_text(&reason));
                None
            }
            _ => unreachable!("validated range reply"),
        };
        if let Some(progress) = progress {
            let _ = writeln!(
                text,
                "\nAuthorized sources: {} · pending: {} · discovery complete: {} · capacity limited: {}",
                progress.visible_sources,
                progress.pending_sources,
                progress.discovery_complete,
                progress.capacity_limited
            );
            if progress.metadata_unavailable {
                text.push_str("Saved metadata unavailable; refresh discovery to retry.\n");
            }
            if let Some(after) = progress.continuation {
                items.push((
                    "Continue indexing".into(),
                    Action::HistoryRequest(Box::new(Request::Discover {
                        scope: scope.clone(),
                        after: Some(after),
                    })),
                ));
            }
        }
        items.push((
            "Refresh saved source discovery".into(),
            Action::HistoryRequest(Box::new(Request::Discover {
                scope: scope.clone(),
                after: None,
            })),
        ));
        if matches!(scope, QueryScope::Workspace { .. }) {
            items.push((
                "Reset this workspace index".into(),
                Action::HistoryRequest(Box::new(Request::Reset {
                    scope: scope.clone(),
                    after: None,
                })),
            ));
        }
        items.push((
            "Inspect source coverage".into(),
            Action::HistoryRequest(Box::new(Request::Progress { scope, after: None })),
        ));
        if let Some(query) = &self.history_query {
            items.push((
                "Search indexed text again".into(),
                Action::HistoryRequest(Box::new(query.clone())),
            ));
        }
        self.state.open_detail(text);
        self.state.detail_actions = Some(Menu {
            title: "History sources and matches".into(),
            items,
            selected: 0,
        });
    }
}
