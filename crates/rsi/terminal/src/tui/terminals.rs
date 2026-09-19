//! Finite terminal management; the TUI never becomes a PTY writer.
use super::{Action, Client, Menu, Update, error};
use rsi_session_protocol::terminal::{Operation, Reply, Request, Terminal};
impl Client {
    pub(super) fn terminal_menu(&mut self, close: Option<String>, all: bool) {
        let handle = self.handle.clone();
        self.spawn_detail(async move {
            if all || close.is_some() {
                handle
                    .terminal(Request::Operate {
                        operation: close.map_or(Operation::CloseAll, |terminal| Operation::Close {
                            terminal,
                        }),
                    })
                    .await
                    .map_err(error)?;
            }
            let Reply::List(terminals) = handle
                .terminal(Request::Operate {
                    operation: Operation::List,
                })
                .await
                .map_err(error)?
            else {
                return Err(error("Invalid terminal roster"));
            };
            if terminals.is_empty() {
                return Ok(Update::Notice(
                    "No terminals are open in this Session".into(),
                ));
            }
            let mut items = terminals
                .into_iter()
                .map(|terminal| {
                    (
                        format!("{} · {:?}", terminal.id, terminal.phase),
                        Action::TerminalStatus(terminal),
                    )
                })
                .collect::<Vec<_>>();
            items.push(("Refresh terminals".into(), Action::Terminals));
            items.push(("Close all terminals".into(), Action::CloseTerminals));
            Ok(Update::Menu(Menu {
                title: "Session terminals".into(),
                selected: 0,
                items,
            }))
        });
    }
    pub(super) fn terminal_status(&mut self, terminal: Terminal) {
        let label = format!(
            "{} · {} × {} · {:?} · {}",
            terminal.id,
            terminal.size.columns,
            terminal.size.rows,
            terminal.phase,
            if terminal.controller.is_some() {
                "Controlled by an attached view"
            } else {
                "No attached controller"
            }
        );
        self.state.menu = Some(Menu {
            title: label,
            selected: 0,
            items: vec![
                ("Refresh terminals".into(), Action::Terminals),
                (
                    "Close this terminal".into(),
                    Action::CloseTerminal(terminal.id),
                ),
            ],
        });
    }
}
