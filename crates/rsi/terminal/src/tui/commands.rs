use super::*;

impl Client {
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
                && self.state.editor.text.is_empty()
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
