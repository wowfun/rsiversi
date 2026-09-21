use super::*;
use rsi_navigation_api::attention::{Position, Target};
impl Ui {
    pub fn open_attention(&mut self) {
        self.active = true;
        if self.work.is_some() {
            self.deferred_open = Some(Choice::Attention);
            return;
        }
        self.attention_menu = true;
        self.menu = true;
        self.detail = None;
        let Some(client) = self.attention.clone() else {
            self.status = "Attention navigation unavailable".into();
            return;
        };
        self.remember();
        let previous = self.controller.take();
        self.changes = None;
        self.work = Some(Box::pin(async move {
            if let Some(previous) = previous {
                previous.retire().await;
            }
            let page = client.read().await.map_err(|error| error.to_string())?;
            let mut items = vec![("Refresh activity".into(), Choice::Attention)];
            for row in page.entries {
                let label = match &row.position.conversation {
                    rsi_conversation::ConversationIdentity::Native(id) => format!("Native {id}"),
                    rsi_conversation::ConversationIdentity::External(id) => {
                        format!("External {}", id.as_str())
                    }
                };
                if row.targets.is_empty() {
                    items.push((
                        format!("{:?} · {label}", row.status),
                        Choice::AttentionOpen(row.position, None),
                    ));
                } else {
                    for target in row.targets {
                        let label = match &target {
                            Target::Native {
                                request: rsi_session_protocol::ActivityRequest::Question { .. },
                            } => format!("Answer question · {label}"),
                            _ => format!("Review permission · {label}"),
                        };
                        items.push((
                            label,
                            Choice::AttentionOpen(row.position.clone(), Some(target)),
                        ));
                    }
                }
            }
            // The API caps metadata; this menu additionally caps visible choices.
            let truncated = page.truncated || items.len() > 257;
            items.truncate(257);
            Ok(Event::Attention(items, truncated))
        }));
    }
    pub(super) fn attention_open(&mut self, position: Position, target: Option<Target>) {
        match &position.conversation {
            rsi_conversation::ConversationIdentity::Native(_) => {
                let request = match target {
                    Some(Target::Native { request }) => Some(request),
                    None => None,
                    _ => {
                        self.status = "Attention backend changed".into();
                        return;
                    }
                };
                self.work = Some(Box::pin(
                    async move { Ok(Event::Native(position, request)) },
                ));
            }
            rsi_conversation::ConversationIdentity::External(id) => {
                let (Some(service), Some(client)) = (self.service.clone(), self.attention.clone())
                else {
                    return;
                };
                let id = id.clone();
                let execution = self.execution.clone();
                let old = self.controller.take();
                self.changes = None;
                self.attention_menu = false;
                self.remember();
                self.permission_focus = target;
                self.work = Some(Box::pin(async move {
                    if let Some(old) = old {
                        old.retire().await;
                    }
                    let controller = ExternalController::attach(service, execution, id)
                        .await
                        .map_err(|error| error.to_string())?;
                    let notice = client.mark_read(position).await.err().map(|error| {
                        format!("Conversation opened; read position was not confirmed: {error}")
                    });
                    Ok(Event::Attached(controller, notice))
                }));
            }
        }
    }
    pub(super) fn focus_permission(&mut self, target: &Target) {
        let Target::External {
            generation,
            request,
        } = target
        else {
            return;
        };
        let live = self.controller.as_ref().is_some_and(|controller| {
            controller
                .view()
                .observed
                .permissions
                .iter()
                .any(|p| &p.generation == generation && &p.id == request)
        });
        if !live {
            self.status = "Permission is no longer pending".into();
            return;
        }
        self.controls();
        if let Some(index)=self.items.iter().position(|(_,choice)|matches!(choice,Choice::Control(ExternalCommand::Answer {generation:current,permission,..}) if current==generation && permission==request)) {self.selected=index;}
    }
    pub fn acknowledge(
        &self,
        position: Position,
    ) -> BoxFuture<'static, std::result::Result<(), String>> {
        let client = self.attention.clone();
        Box::pin(async move {
            client
                .ok_or("Attention navigation unavailable")?
                .mark_read(position)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }
}
