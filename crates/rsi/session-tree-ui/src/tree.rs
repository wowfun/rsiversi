use super::{
    Operation, Result, SessionId, TREE_PAGE_ROWS, TreeReader, UiElement, UiError, UiView,
    action_error, button, field, history,
};
use rsi_agent_store_protocol::{
    StoreAgentDescendantStatus, StoreAgentSessionStatus, StoreAgentSubtreeSnapshot,
};
use rsi_meta::Execution;
use rsi_session_protocol::SessionError;

pub(super) async fn read(
    reader: &TreeReader,
    execution: &Execution,
    operation: Operation,
) -> Result<UiView> {
    let root_id = reader.controller.session_id();
    let root = rsi_client::read_with_capacity_retry(execution, || reader.service.attach(root_id))
        .await
        .map_err(action_error)?;
    let snapshot = match rsi_client::read_with_capacity_retry(execution, || root.inspect()).await {
        Ok(snapshot) => snapshot,
        Err(SessionError::NotFound(_)) if matches!(&operation, Operation::Tree { selected, after: None } if selected.as_ref().is_none_or(|selected| selected == root_id)) =>
        {
            root.draft_snapshot().await.map_err(action_error)?;
            return Ok(UiView {
                title: "Agent tree".into(),
                elements: vec![
                    field("Root Session", root_id.to_string()),
                    UiElement::Text {
                        text: "This draft has no durable Agent tree yet.".into(),
                    },
                    button(
                        reader,
                        "Refresh agent tree",
                        Operation::Tree {
                            selected: None,
                            after: None,
                        },
                    ),
                ],
            });
        }
        Err(error) => return Err(action_error(error)),
    };
    let selected = match &operation {
        Operation::Tree { selected, .. } => selected.as_ref().unwrap_or(root_id),
        Operation::History { selected, .. }
        | Operation::Sources { selected, .. }
        | Operation::Source { selected, .. } => selected,
    };
    let status = member(&snapshot.tree, selected)?;
    let mut elements = breadcrumbs(reader, &snapshot.tree, selected);
    elements.push(field("Session", selected.to_string()));
    elements.push(field("Activity snapshot", activity(status)));
    if let Operation::Tree { after, .. } = &operation {
        return Ok(children_view(
            reader,
            &snapshot.tree,
            selected,
            after.as_ref(),
            elements,
        ));
    }
    let handle = if selected == root_id {
        root
    } else {
        rsi_client::read_with_capacity_retry(execution, || reader.service.attach(selected))
            .await
            .map_err(action_error)?
    };
    history::read(reader, execution, handle.as_ref(), operation, elements).await
}

fn descendant<'a>(
    tree: &'a StoreAgentSubtreeSnapshot,
    id: &SessionId,
) -> Option<&'a StoreAgentDescendantStatus> {
    tree.descendants
        .binary_search_by(|child| child.status.session_id.cmp(id))
        .ok()
        .map(|index| &tree.descendants[index])
}
fn member<'a>(
    tree: &'a StoreAgentSubtreeSnapshot,
    id: &SessionId,
) -> Result<&'a StoreAgentSessionStatus> {
    if id == &tree.session.session_id {
        return Ok(&tree.session);
    }
    descendant(tree, id)
        .map(|child| &child.status)
        .ok_or_else(|| UiError::Invalid("Selected Session is outside this Agent tree".into()))
}
fn breadcrumbs(
    reader: &TreeReader,
    tree: &StoreAgentSubtreeSnapshot,
    selected: &SessionId,
) -> Vec<UiElement> {
    let mut path = Vec::new();
    let mut id = selected;
    while let Some(child) = descendant(tree, id) {
        path.push(child);
        id = &child.parent_session_id;
    }
    let label = std::iter::once("Root")
        .chain(path.iter().rev().map(|child| child.task_name.as_str()))
        .collect::<Vec<_>>()
        .join(" › ");
    let mut elements = vec![
        field("Agent path", label),
        button(
            reader,
            "Root agent",
            Operation::Tree {
                selected: None,
                after: None,
            },
        ),
    ];
    for child in path.into_iter().rev() {
        elements.push(button(
            reader,
            format!("Agent: {}", child.task_name),
            Operation::Tree {
                selected: Some(child.status.session_id.clone()),
                after: None,
            },
        ));
    }
    elements
}
fn activity(status: &StoreAgentSessionStatus) -> String {
    let mut flags = Vec::new();
    if status.has_open_turn {
        flags.push("Turn open");
    }
    if status.has_active_activation {
        flags.push("activation present");
    }
    if status.has_waking_message {
        flags.push("queued work");
    }
    if flags.is_empty() {
        flags.push("Idle");
    }
    format!(
        "{} · control {}",
        flags.join(" · "),
        status.durable_control_seq
    )
}

fn children_view(
    reader: &TreeReader,
    tree: &StoreAgentSubtreeSnapshot,
    selected: &SessionId,
    after: Option<&SessionId>,
    mut elements: Vec<UiElement>,
) -> UiView {
    let children: Vec<_> = tree
        .descendants
        .iter()
        .filter(|child| &child.parent_session_id == selected)
        .collect();
    let start = after.map_or(0, |after| {
        children.partition_point(|child| &child.status.session_id <= after)
    });
    let end = (start + TREE_PAGE_ROWS).min(children.len());
    elements.push(field(
        "Direct children",
        format!(
            "{}–{} of {}",
            if end == start { 0 } else { start + 1 },
            if end == start { 0 } else { end },
            children.len()
        ),
    ));
    for child in &children[start..end] {
        elements.push(field(&child.task_name, activity(&child.status)));
        elements.push(button(
            reader,
            format!("Inspect {}", child.task_name),
            Operation::Tree {
                selected: Some(child.status.session_id.clone()),
                after: None,
            },
        ));
    }
    if start > 0 {
        elements.push(button(
            reader,
            "First children",
            Operation::Tree {
                selected: Some(selected.clone()),
                after: None,
            },
        ));
    }
    if end < children.len() {
        elements.push(button(
            reader,
            "More children",
            Operation::Tree {
                selected: Some(selected.clone()),
                after: Some(children[end - 1].status.session_id.clone()),
            },
        ));
    }
    elements.push(button(
        reader,
        "Read conversation",
        Operation::History {
            selected: selected.clone(),
            before: None,
            watermark: None,
        },
    ));
    elements.push(button(
        reader,
        "Refresh agent tree",
        Operation::Tree {
            selected: Some(selected.clone()),
            after: after.cloned(),
        },
    ));
    UiView {
        title: "Agent tree".into(),
        elements,
    }
}
