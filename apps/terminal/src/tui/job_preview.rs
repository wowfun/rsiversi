//! One visible preview, with cancellation and no retained stream subscription.
use super::{Action, Client, Menu, Update, WorkKind, error};
use rsi_agent_turn_protocol::{JobPreviewRequest, TurnJobsRequest};
use tokio::time::Instant;

#[derive(Debug)]
pub(super) struct Preview {
    request: JobPreviewRequest,
    pending: bool,
    next: Instant,
}
impl Preview {
    pub(super) fn new(request: JobPreviewRequest) -> Self {
        Self {
            request,
            pending: false,
            next: Instant::now(),
        }
    }
}
impl Client {
    pub(super) fn job_menu(&mut self, request: Option<TurnJobsRequest>) {
        let Some(turn) = self
            .inspection
            .as_ref()
            .and_then(|snapshot| snapshot.active_turn_id.clone())
        else {
            self.state.info("No command is running");
            return;
        };
        let request = request.unwrap_or(TurnJobsRequest {
            turn_id: turn,
            generation: None,
            after: None,
            limit: 32,
        });
        let handle = self.handle.clone();
        self.spawn_detail(async move {
            let snapshot = handle.read_jobs(request).await.map_err(error)?;
            let page = snapshot.page();
            let mut items = Vec::new();
            for job in &page.jobs {
                if let Some(effect) = &job.origin {
                    items.push((
                        format!("{} · {} · {:?}", job.name, job.id, job.status),
                        Action::Preview(JobPreviewRequest {
                            turn_id: page.turn_id.clone(),
                            generation: page.generation,
                            job_id: job.id.clone(),
                            effect_id: rsi_agent_session_protocol::EffectId::new(effect.clone())
                                .map_err(error)?,
                            stdout_bytes: 16 * 1024,
                            stderr_bytes: 16 * 1024,
                        }),
                    ));
                }
            }
            if page.has_more {
                items.push((
                    "More commands".into(),
                    Action::Jobs(Some(TurnJobsRequest {
                        turn_id: page.turn_id.clone(),
                        generation: Some(page.generation),
                        after: page.jobs.last().map(|job| job.id.clone()),
                        limit: 32,
                    })),
                ));
            }
            if items.is_empty() {
                return Ok(Update::Notice("No live command output is available".into()));
            }
            Ok(Update::Menu(Menu {
                title: "Command output".into(),
                selected: 0,
                items,
            }))
        });
    }
    pub(super) fn poll_preview(&mut self, now: Instant) {
        if self.tasks.len() >= 8 {
            return;
        }
        let Some(preview) = &mut self.state.preview else {
            return;
        };
        if preview.pending
            || now < preview.next
            || self.state.detail.is_none()
            || self.state.menu.is_some()
        {
            return;
        }
        let request = preview.request.clone();
        preview.next = now + std::time::Duration::from_millis(250);
        let handle = self.handle.clone();
        let stop = self.state.detail_stop.clone();
        let admitted = self.spawn_as(WorkKind::Detail, async move {
            tokio::select! {
                biased;
                ()=stop.cancelled()=>Err(error("Preview closed")),
                result=handle.peek_job(request)=>Ok(Update::Preview(result)),
            }
        });
        if let Some(preview) = &mut self.state.preview {
            preview.pending = admitted;
        }
    }
}

pub(super) fn show(
    state: &mut super::State,
    result: rsi_session_protocol::Result<rsi_agent_turn_protocol::JobPreviewPage>,
) {
    let Some(preview) = &mut state.preview else {
        return;
    };
    preview.pending = false;
    let page = match result {
        Ok(page) => page,
        Err(rsi_session_protocol::SessionError::Api(rsi_api_protocol::ApiError::Capacity)) => {
            return;
        }
        Err(problem) => {
            state.preview = None;
            let detail = state.detail.get_or_insert_with(String::new);
            let _ = std::fmt::Write::write_fmt(
                detail,
                format_args!(
                    "\n\nLive output unavailable: {problem}\nThe transcript retains the tool result and completed-output references."
                ),
            );
            return;
        }
    };
    if page.request != preview.request {
        return;
    }
    let Some(output) = page.preview else {
        state.preview = None;
        state.detail.get_or_insert_with(String::new).push_str(
            "\n\nLive output is no longer retained. Open the tool result in the transcript.",
        );
        return;
    };
    let mut text = format!("{} · {:?}\n", page.request.job_id, output.status);
    let mut items = Vec::new();
    for (name, stream, maximum) in [
        ("stdout", &output.stdout, page.request.stdout_bytes),
        ("stderr", &output.stderr, page.request.stderr_bytes),
    ] {
        let bytes = match stream.bytes(maximum) {
            Ok(bytes) => bytes,
            Err(problem) => {
                state.preview = None;
                state.notice(problem.to_string());
                return;
            }
        };
        if !bytes.is_empty() {
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!(
                    "\n{name} · bytes {}..{}{}\n{}\n",
                    stream.start,
                    stream.end,
                    if stream.truncated {
                        " · earlier bytes omitted"
                    } else {
                        ""
                    },
                    super::super::terminal_text(&String::from_utf8_lossy(&bytes))
                ),
            );
        }
        if let Some(id) = &stream.full_output {
            items.push((format!("Full {name}"), Action::Output(id.clone(), 0)));
        }
    }
    // At most 32 KiB raw input; replacement UTF-8 expansion remains below 256 KiB.
    state.detail = Some(text);
    state.detail_actions = (!items.is_empty()).then_some(Menu {
        title: "Completed output".into(),
        selected: 0,
        items,
    });
    if output.status.is_terminal() {
        state.preview = None;
    }
}
