use super::{Action, Client, Menu, Update, error};
use rsi_history_api::{ConversationIdentity, Coverage, Reply, Request, Scope};
use sha2::{Digest as _, Sha256};
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
    pub(super) fn search_history(&mut self, conversation: ConversationIdentity, query: String) {
        let workspace = rsi_workspace_protocol::WorkspaceId::parse(hex::encode(Sha256::digest(
            self.state.header.canonical_cwd().as_bytes(),
        )))
        .expect("canonical workspace digest");
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
        let scope = request.scope().clone();
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
        }
        self.state
            .info("↑/↓ scroll · ←/→ original pages · Enter actions · Esc returns to draft");
    }
}
