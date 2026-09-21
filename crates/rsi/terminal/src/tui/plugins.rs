//! Native projection of the shared configuration-granted plugin workbench.
use super::*;
use rsi_workbench_ui::{PluginsCommand, PluginsView};
impl Client {
    pub(super) fn plugins(&mut self, command: PluginsCommand) {
        let Some(feature) = self.plugins.clone() else {
            self.state
                .notice("Plugin status is unavailable on this connection");
            return;
        };
        self.state.open_detail("Reading plugin status…".into());
        self.spawn_detail(async move {
            // Failed reads publish a redacted unavailable state, with a refresh action.
            let _result = feature.command(command).await;
            Ok(Update::Plugins(Box::new(feature.snapshot())))
        });
        self.state.info("Reading plugin status…");
    }
}
impl State {
    #[expect(
        clippy::too_many_lines,
        reason = "One exhaustive projection keeps related state transitions and ownership visible together"
    )]
    pub(super) fn show_plugins(&mut self, view: PluginsView) {
        use std::fmt::Write as _;
        let mut text = String::from("Plugins");
        self.open_detail(String::new());
        let mut actions = vec![(
            "Refresh plugin status".into(),
            Action::Plugins(PluginsCommand::Refresh),
        )];
        if view.leaves.available {
            actions.push(("Manage Host Profiles".into(), Action::Profiles));
        }
        actions.push((
            "Host plugins".into(),
            Action::Plugins(PluginsCommand::Select {
                target: rsi_configuration_api::PluginStatusTarget::Host,
            }),
        ));
        actions.push((
            "Current preset configuration".into(),
            Action::Plugins(PluginsCommand::Select {
                target: rsi_configuration_api::PluginStatusTarget::Preset {
                    id: self.header.agent_preset_id().to_string(),
                },
            }),
        ));
        if let Ok(header_key) = self.header.fingerprint() {
            actions.push((
                "Session resident generation".into(),
                Action::Plugins(PluginsCommand::Select {
                    target: rsi_configuration_api::PluginStatusTarget::Session {
                        target: rsi_session_protocol::SessionTarget {
                            session_id: self.header.session_id().clone(),
                            header_key,
                        },
                    },
                }),
            ));
        }
        let _ = writeln!(text, "\nSource: {:?}", view.target);
        for guidance in &view.guidance {
            let _ = writeln!(text, "{guidance}");
        }
        if view.exa_available {
            text.push_str("\nWeb retrieval · settings: rsi.retrieval\nweb_fetch and web_search start disabled. Enable them for new Sessions.\nHistory displays recorded sources without fetching them again.\n");
            actions.push((
                "Read Exa credential status".into(),
                Action::Plugins(PluginsCommand::ExaStatus),
            ));
            actions.push((
                "Set Exa credential…".into(),
                Action::IntegrationCredential(setup::IntegrationCredential::Exa),
            ));
            actions.push((
                "Remove Exa credential".into(),
                Action::Plugins(PluginsCommand::ExaUnset),
            ));
            if let Some(status) = &view.exa_credential {
                let _ = writeln!(
                    text,
                    "Exa credential · {:?} · {}",
                    status.availability,
                    if status.editable {
                        "editable"
                    } else {
                        "read only"
                    }
                );
            }
            if let Some(notice) = &view.exa_notice {
                let _ = writeln!(text, "{notice}");
            }
        }
        if view.mcp_available {
            actions.push((
                "Read MCP status".into(),
                Action::Plugins(PluginsCommand::McpStatus),
            ));
            actions.push((
                "Apply and refresh HTTP MCP endpoints".into(),
                Action::Plugins(PluginsCommand::McpRefresh { server: None }),
            ));
            text.push_str("\nMCP · HTTP settings: rsi.mcp; stdio: Local Host Profile\nNew Sessions use verified catalogs. Existing Sessions retain saved schemas.\n");
            if let Some(mcp) = &view.mcp {
                let _ = writeln!(
                    text,
                    "New catalog: {} · Saved settings: {}",
                    if mcp.fresh_ready {
                        "ready".into()
                    } else {
                        mcp.fresh_error
                            .map_or_else(|| "unavailable".into(), |error| error.to_string())
                    },
                    if mcp.settings_pending {
                        "apply needed"
                    } else {
                        "applied"
                    }
                );
                for server in &mcp.servers {
                    let _ = writeln!(
                        text,
                        "\n{} · {:?} · {} · epoch {}",
                        server.id,
                        server.transport,
                        if server.ready {
                            "ready".into()
                        } else {
                            server
                                .error
                                .map_or_else(|| "unavailable".into(), |error| error.to_string())
                        },
                        server.epoch
                    );
                    if let Some(digest) = &server.last_verified_sha256 {
                        let _ = writeln!(text, "  Last verified: {}", &digest[..12]);
                    }
                    for tool in &server.tools {
                        let _ = writeln!(
                            text,
                            "  {} {}",
                            if tool.selected {
                                "[selected]"
                            } else {
                                "[available]"
                            },
                            tool.name
                        );
                    }
                    if server.enabled {
                        actions.push((
                            format!("Refresh MCP {}", server.id),
                            Action::Plugins(PluginsCommand::McpRefresh {
                                server: Some(server.id.clone()),
                            }),
                        ));
                    }
                    if let Some(reference) = &server.credential {
                        let target = rsi_configuration_api::McpCredentialTarget {
                            server: server.id.clone(),
                            reference: reference.clone(),
                        };
                        actions.push((
                            format!("Credential for {} · {}", server.id, reference.slot),
                            Action::IntegrationCredential(setup::IntegrationCredential::Mcp(
                                target.clone(),
                            )),
                        ));
                        actions.push((
                            format!("Remove credential for {}", server.id),
                            Action::Plugins(PluginsCommand::McpCredentialUnset { target }),
                        ));
                    }
                }
            }
            if let Some(notice) = &view.mcp_notice {
                let _ = writeln!(text, "\nMCP: {notice}");
            }
        }
        if let Some(page) = view.page {
            let _ = write!(
                text,
                "\nDesired {} · Observed {}\nProfile {:?} · Watcher {:?}\nEntries {}–{} of {}\n",
                page.desired_revision,
                page.observed_revision,
                page.health,
                page.watcher,
                page.offset + usize::from(!page.plugins.is_empty()),
                page.offset + page.plugins.len(),
                page.total
            );
            let _ = writeln!(
                text,
                "\nAvailability: {:?} · Preset root: {:?}",
                page.context.availability, page.context.preset_source
            );
            if let Some(digest) = page.context.source_digest {
                let _ = writeln!(text, "Source digest: {digest}");
            }
            for row in page.plugins {
                let _ = writeln!(
                    text,
                    "  Origin: {:?} · Reasons: {:?}",
                    row.origin, row.diagnostics
                );
                let observed = row.observed.map_or_else(
                    || "Not observed".into(),
                    |state| format!("{:?} · {}", state.state, state.plugin),
                );
                let _ = write!(
                    text,
                    "\n{}\n  Desired: {} · {}\n  Observed: {}\n",
                    row.instance,
                    row.desired_plugin.as_deref().unwrap_or("Removed"),
                    if row.enabled { "enabled" } else { "disabled" },
                    observed
                );
            }
            self.detail_previous = (page.offset > 0).then(|| {
                Action::Plugins(PluginsCommand::Page {
                    ticket: view.ticket.clone(),
                    offset: page.offset.saturating_sub(32),
                })
            });
            self.detail_next = page.next_offset.map(|offset| {
                Action::Plugins(PluginsCommand::Page {
                    ticket: view.ticket,
                    offset,
                })
            });
        }
        if let Some(message) = view.diagnostic {
            let _ = write!(text, "\nUnavailable: {message}");
        }
        self.detail = Some(super::super::terminal_text(&text));
        self.detail_actions = Some(Menu {
            title: "Plugins".into(),
            selected: 0,
            items: actions,
        });
        self.info("←/→ pages · ↑/↓ scroll · Enter actions · Esc returns to draft");
    }
}
