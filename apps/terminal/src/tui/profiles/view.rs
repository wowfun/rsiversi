use super::{After, Choice, LeafCommand, Principal, Target, Ui, wire};
impl Ui {
    pub(super) fn browse(&mut self) {
        let mut items = vec![(
            "Browse Host Profiles".into(),
            Choice::Run(
                LeafCommand::Read {
                    query: wire::CatalogRequest::default(),
                },
                After::Browse,
            ),
        )];
        let mut explanation = "Single leaf changes require explicit grants. Existing conversation generations remain pinned.".to_owned();
        if let Some(catalog) = &self.view.catalog {
            explanation = format!(
                "Caller {:?} · {}",
                catalog.principal,
                self.view
                    .query
                    .profile
                    .as_deref()
                    .unwrap_or("Select a writable Host Profile")
            );
            for profile in &catalog.profiles {
                items.push((
                    format!("Open Profile {profile}"),
                    Choice::Run(
                        LeafCommand::Read {
                            query: wire::CatalogRequest {
                                profile: Some(profile.clone()),
                                after: None,
                            },
                        },
                        After::Browse,
                    ),
                ));
            }
            for leaf in &catalog.leaves {
                items.push((
                    format!(
                        "{} · {}",
                        leaf.target.leaf,
                        if leaf.enabled { "enabled" } else { "disabled" }
                    ),
                    Choice::Leaf(leaf.target.clone()),
                ));
            }
            if let Some(next) = &catalog.next {
                items.push((
                    "Next source page".into(),
                    Choice::Run(
                        LeafCommand::Read {
                            query: wire::CatalogRequest {
                                profile: self.view.query.profile.clone(),
                                after: Some(next.clone()),
                            },
                        },
                        After::Browse,
                    ),
                ));
            }
        }
        for preview in &self.view.previews {
            items.push((
                format!(
                    "Review {:?} · {} / {}",
                    preview.operation, preview.target.profile, preview.target.leaf
                ),
                Choice::Review(preview.clone()),
            ));
        }
        if !self.view.receipts.is_empty() {
            items.push((
                "Recover original source receipts".into(),
                Choice::Receipts(0),
            ));
        }
        if self.view.can_grant {
            items.push((
                "Read explicit Profile grants".into(),
                Choice::Run(LeafCommand::Grants, After::Grants(0)),
            ));
        }
        items.push(("Close Host Profiles".into(), Choice::Close));
        self.menu("Host Profiles", explanation, items);
    }
    pub(super) fn leaf(&mut self, target: &Target) {
        let Some(leaf) = self
            .view
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.leaves.iter().find(|leaf| &leaf.target == target))
            .cloned()
        else {
            self.browse();
            return;
        };
        let mut items = vec![];
        for kind in &leaf.allowed {
            let (label, choice) = match kind {
                wire::ChangeKind::Enable => (
                    "Preview enable",
                    Choice::Run(
                        LeafCommand::Enabled {
                            target: target.clone(),
                            enabled: true,
                        },
                        After::Review,
                    ),
                ),
                wire::ChangeKind::Disable => (
                    "Preview disable",
                    Choice::Run(
                        LeafCommand::Enabled {
                            target: target.clone(),
                            enabled: false,
                        },
                        After::Review,
                    ),
                ),
                wire::ChangeKind::Configuration => (
                    "Replace complete configuration",
                    Choice::Configuration(target.clone()),
                ),
            };
            items.push((label.into(), choice));
        }
        if self.view.can_grant {
            items.push((
                "Grant an exact change".into(),
                Choice::Run(LeafCommand::Grants, After::Grant(target.clone())),
            ));
        }
        items.push(("Back to source choices".into(), Choice::Browse));
        self.menu(
            format!("Host leaf · {}", target.leaf),
            format!(
                "{} · {}\nOwn enabled: {} · ancestors: {} · Current grants: {:?}",
                target.profile, leaf.plugin, leaf.enabled, leaf.effective_enabled, leaf.allowed
            ),
            items,
        );
    }
    pub(super) fn review(&mut self, preview: &wire::Preview) {
        let uncertain = self.view.receipt.as_ref().is_some_and(|receipt| {
            receipt.preview.ticket == preview.ticket
                && matches!(
                    receipt.outcome,
                    wire::Outcome::Pending | wire::Outcome::Unknown
                )
        });
        let mut items = vec![];
        if uncertain {
            items.push((
                "Query original receipt".into(),
                Choice::Run(
                    LeafCommand::Receipt {
                        ticket: preview.ticket.clone(),
                    },
                    After::Receipt,
                ),
            ));
        } else {
            items.push((
                "Save reviewed change".into(),
                Choice::Run(
                    LeafCommand::Commit {
                        ticket: preview.ticket.clone(),
                        digest: preview.digest.clone(),
                    },
                    After::Receipt,
                ),
            ));
            items.push((
                "Discard proposal".into(),
                Choice::Run(
                    LeafCommand::Discard {
                        ticket: preview.ticket.clone(),
                    },
                    After::Browse,
                ),
            ));
        }
        items.push(("Back to source choices".into(), Choice::Browse));
        self.menu("Prepared Host Profile change", format!("{} / {} · {}\n{:?} · own enabled {} → {} · with ancestors {}\nReview: {}\nTicket: {}\nConfiguration values remain hidden. Saving and runtime application are separate.", preview.target.profile, preview.target.leaf, preview.plugin, preview.operation, preview.previous_enabled, preview.enabled, preview.effective_enabled, preview.digest, preview.ticket), items);
        self.details();
    }
    pub(super) fn receipt(&mut self) {
        let Some(receipt) = self.view.receipt.clone() else {
            self.browse();
            return;
        };
        let result = match &receipt.outcome {
            wire::Outcome::Saved {
                directory_synced,
                application,
            } => format!(
                "Source: saved\nDirectory synced: {directory_synced}\nCurrent runtime: {application:?}"
            ),
            wire::Outcome::Pending => "Source: pending".into(),
            wire::Outcome::Unknown => {
                "Source: unknown; query the original receipt, never replay".into()
            }
            wire::Outcome::Failed { failure } => {
                format!("Source: rejected before publication · {failure:?}")
            }
        };
        self.menu(
            "Profile source receipt",
            format!(
                "{} / {}\n{}\nTicket: {}",
                receipt.preview.target.profile,
                receipt.preview.target.leaf,
                result,
                receipt.preview.ticket
            ),
            vec![
                (
                    "Query original receipt".into(),
                    Choice::Run(
                        LeafCommand::Receipt {
                            ticket: receipt.preview.ticket,
                        },
                        After::Receipt,
                    ),
                ),
                ("Back to source choices".into(), Choice::Browse),
            ],
        );
        self.details();
    }
    pub(super) fn grant_kind(&mut self, target: &Target) {
        let items = [
            wire::ChangeKind::Enable,
            wire::ChangeKind::Disable,
            wire::ChangeKind::Configuration,
        ]
        .into_iter()
        .map(|kind| {
            (
                format!("Grant {kind:?}"),
                Choice::GrantPrincipal(target.clone(), kind),
            )
        })
        .chain([("Back to source choices".into(), Choice::Browse)])
        .collect();
        self.menu(
            "Explicit leaf grant",
            format!(
                "{} / {} · select one operation",
                target.profile, target.leaf
            ),
            items,
        );
    }
    pub(super) fn grant_principal(&mut self, target: &Target, operation: wire::ChangeKind) {
        let Some(grants) = &self.view.grants else {
            self.browse();
            return;
        };
        let items = vec![
            (
                "Grant to Local".into(),
                Choice::Run(
                    LeafCommand::Grant {
                        revision: grants.revision.clone(),
                        scope: wire::Grant {
                            principal: Principal::Local,
                            target: target.clone(),
                            operation,
                        },
                        granted: true,
                    },
                    After::Leaf(target.clone()),
                ),
            ),
            (
                "Grant to an Agent Session ID".into(),
                Choice::GrantInput(target.clone(), operation, true),
            ),
            (
                "Grant to a registered Device ID".into(),
                Choice::GrantInput(target.clone(), operation, false),
            ),
            (
                "Back to operation choices".into(),
                Choice::GrantKind(target.clone()),
            ),
        ];
        self.menu(
            "Select grant principal",
            format!(
                "{} / {} · {:?}\nGrant revision {}. Authority is separate for each principal.",
                target.profile, target.leaf, operation, grants.revision
            ),
            items,
        );
    }
    pub(super) fn grants(&mut self, offset: usize) {
        let mut items = vec![];
        if let Some(grants) = &self.view.grants {
            for scope in grants.scopes.iter().skip(offset).take(64) {
                items.push((
                    format!(
                        "Revoke {:?} · {} / {} · {:?}",
                        scope.principal, scope.target.profile, scope.target.leaf, scope.operation
                    ),
                    Choice::Run(
                        LeafCommand::Grant {
                            revision: grants.revision.clone(),
                            scope: scope.clone(),
                            granted: false,
                        },
                        After::Grants(0),
                    ),
                ));
            }
            if offset + 64 < grants.scopes.len() {
                items.push(("Next grants page".into(), Choice::Grants(offset + 64)));
            }
        }
        items.push((
            "Refresh grants".into(),
            Choice::Run(LeafCommand::Grants, After::Grants(0)),
        ));
        items.push(("Back to source choices".into(), Choice::Browse));
        self.menu(
            "Explicit Profile grants",
            "Select an exact grant to revoke it and drain previously admitted work.",
            items,
        );
    }
    pub(super) fn receipts(&mut self, offset: usize) {
        let mut items = self
            .view
            .receipts
            .iter()
            .skip(offset)
            .take(64)
            .map(|ticket| {
                (
                    format!("Read receipt {ticket}"),
                    Choice::Run(
                        LeafCommand::Receipt {
                            ticket: ticket.clone(),
                        },
                        After::Receipt,
                    ),
                )
            })
            .collect::<Vec<_>>();
        if offset + 64 < self.view.receipts.len() {
            items.push(("Next receipts page".into(), Choice::Receipts(offset + 64)));
        }
        items.push(("Back to source choices".into(), Choice::Browse));
        self.menu(
            "Original Profile receipts",
            "Reading an original ticket never submits its write again.",
            items,
        );
    }
}
