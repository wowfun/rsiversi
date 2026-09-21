use crate::{CONTROL, Error, Result, State, operations};
use rsi_acp_protocol::{configuration::ConfigSelection, schema};
use serde_json::{Value, json};

fn options(response: &Value) -> Result<Vec<schema::SessionConfigOption>> {
    let Some(value) = response
        .get("configOptions")
        .filter(|value| !value.is_null())
    else {
        return Ok(Vec::new());
    };
    let values: Vec<schema::SessionConfigOption> =
        serde_json::from_value(value.clone()).map_err(|_| Error::Input)?;
    if values.len() > 64 {
        return Err(Error::Input);
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut choices = 0_usize;
    for value in &values {
        if value.id.0.is_empty()
            || value.id.0.len() > 256
            || value.id.0.contains('\0')
            || !ids.insert(&value.id.0)
        {
            return Err(Error::Input);
        }
        if let schema::SessionConfigKind::Select(select) = &value.kind {
            choices += match &select.options {
                schema::SessionConfigSelectOptions::Ungrouped(options) => options.len(),
                schema::SessionConfigSelectOptions::Grouped(groups) => {
                    groups.iter().map(|group| group.options.len()).sum()
                }
                _ => return Err(Error::Unsupported),
            };
        }
    }
    if choices > 4096 {
        return Err(Error::Input);
    }
    Ok(values)
}

fn select<'a>(
    options: &'a [schema::SessionConfigOption],
    selection: &ConfigSelection,
) -> Result<&'a schema::SessionConfigSelect> {
    let option = options
        .iter()
        .find(|option| option.id.0.as_ref() == selection.id)
        .ok_or(Error::Unsupported)?;
    let schema::SessionConfigKind::Select(select) = &option.kind else {
        return Err(Error::Unsupported);
    };
    let contains =
        |option: &schema::SessionConfigSelectOption| option.value.0.as_ref() == selection.value;
    let advertised = match &select.options {
        schema::SessionConfigSelectOptions::Ungrouped(options) => options.iter().any(contains),
        schema::SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .any(|group| group.options.iter().any(contains)),
        _ => false,
    };
    if !advertised {
        return Err(Error::Unsupported);
    }
    Ok(select)
}

pub(super) async fn apply(
    state: &State,
    response: &Value,
    selections: &[ConfigSelection],
) -> Result<()> {
    if selections.is_empty() {
        return Ok(());
    }
    let mut current = options(response)?;
    for (index, selection) in selections.iter().enumerate() {
        select(&current, selection)?;
        let response = operations::request(
            state,
            "session/set_config_option",
            &json!({"sessionId":state.target()?,"configId":selection.id,"value":selection.value}),
            CONTROL,
        )
        .await?;
        current = options(&response)?;
        for applied in &selections[..=index] {
            if select(&current, applied)?.current_value.0.as_ref() != applied.value {
                return Err(Error::Input);
            }
        }
    }
    Ok(())
}
