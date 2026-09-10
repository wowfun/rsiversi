use super::*;

pub(super) enum ExtensionView {
    Menu,
    Detail(String),
}

impl Client {
    pub(super) fn refresh_extensions(&mut self) {
        match self.extension_view.take() {
            Some(ExtensionView::Menu) if self.state.menu.is_some() => self.extension_menu(),
            Some(ExtensionView::Detail(producer)) if self.state.detail.is_some() => {
                self.extension_detail(&producer);
            }
            _ => {}
        }
    }
    pub(super) fn extension_menu(&mut self) {
        self.extension_view = Some(ExtensionView::Menu);
        self.state.menu = Some(Menu {
            title: self.projections.as_ref().map_or_else(
                || "Extension state · waiting for baseline".into(),
                |snapshot| {
                    format!(
                        "Extension state · {:?}{}",
                        snapshot.snapshot().cursor(),
                        if self.projection_notice.is_empty() {
                            ""
                        } else {
                            " · unavailable, last snapshot"
                        }
                    )
                },
            ),
            selected: 0,
            items: self.projections.as_ref().map_or_else(Vec::new, |snapshot| {
                snapshot
                    .snapshot()
                    .entries()
                    .iter()
                    .map(|entry| {
                        (
                            format!(
                                "{}{}",
                                entry.producer(),
                                if entry.failure().is_some() {
                                    " · producer failed"
                                } else {
                                    ""
                                }
                            ),
                            Action::Extension(entry.producer().to_string()),
                        )
                    })
                    .collect()
            }),
        });
    }
    pub(super) fn extension_detail(&mut self, producer: &str) {
        self.extension_view = Some(ExtensionView::Detail(producer.into()));
        let Some(snapshot) = &self.projections else {
            return;
        };
        let Some(entry) = snapshot
            .snapshot()
            .entries()
            .iter()
            .find(|entry| entry.producer().as_str() == producer)
        else {
            return;
        };
        let value = entry.view().map_or_else(
            || format!("Producer failed: {}", entry.failure().unwrap_or_default()),
            |value| transcript::json_window(value.value()),
        );
        self.state.open_detail(format!(
            "{producer} · {:?}\n{}\n\n{value}",
            snapshot.snapshot().cursor(),
            self.projection_notice
        ));
    }

    pub(super) fn command_menu(&mut self) {
        let controller = self.controller.clone();
        self.spawn(async move {
            let commands = controller.commands().await.map_err(error)?;
            Ok(Update::Menu(Menu {
                title: format!("Session commands · {:?}", commands.revision()),
                selected: 0,
                items: commands
                    .commands()
                    .iter()
                    .map(|entry| {
                        (
                            format!("/{} · {}", entry.name(), entry.description()),
                            Action::CommandHelp(entry.name().into(), entry.description().into()),
                        )
                    })
                    .collect(),
            }))
        });
    }
    pub(super) fn command_result(&mut self) {
        let view = self.command.view();
        if let Some(pending) = view.pending {
            self.state
                .notice(format!("Querying command {}", pending.request_id));
            let command = self.command.clone();
            let controller = self.controller.clone();
            self.spawn(async move {
                let receipt = command.refresh(&controller).await.map_err(error)?;
                Ok(Update::Notice(format!(
                    "Command {} · {:?}",
                    receipt.request_id(),
                    receipt.outcome()
                )))
            });
        } else if let Some(receipt) = view.receipt {
            self.state
                .open_detail(serde_json::to_string_pretty(&receipt).expect("receipt serializes"));
        } else {
            self.state.notice("No Session command result");
        }
    }
    pub(super) fn command_finished(
        &mut self,
        result: rsi_session_protocol::Result<rsi_agent_session_protocol::SessionCommandReceipt>,
    ) {
        self.submission.busy = false;
        if let Some(request) = self.submission.request.take() {
            self.owned.remove(&request.message_id);
            if result.is_err()
                && self.state.editor.text().is_empty()
                && let [MessageInput::Text { text }] = request.content.as_slice()
            {
                let _ = self.state.editor.insert(text);
            }
        }
        self.submission.rejected = false;
        match result {
            Ok(receipt) => self.state.notice(format!(
                "Command {} · {:?}",
                receipt.request_id(),
                receipt.outcome()
            )),
            Err(error) => self.state.notice(error.to_string()),
        }
    }
}
