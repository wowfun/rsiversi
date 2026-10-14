//! Product-owned model directory and draft-safe exact ACP selection.
use futures_util::StreamExt as _;
use rsi_acp::server::Failure;
use rsi_acp_protocol::schema;
use rsi_agent_session_protocol::{CommandArguments, DomainRequestId, ModelSelection};
use rsi_ai_protocol::{
    LanguageModels, MAX_LANGUAGE_MODEL_PAGE, ModelRef, ReasoningEffortId, ReasoningEffortProfile,
};
use rsi_session_protocol::SessionHandle;
use serde_json::json;

const MAX_MODEL_CHOICES: usize = 4096;
const DEFAULT_EFFORT: &str = "@default";

async fn selection(session: &dyn SessionHandle) -> Result<ModelSelection, Failure> {
    let mut stream = session
        .observe_projections()
        .await
        .map_err(|_| Failure::Backend)?;
    let snapshot = stream
        .next()
        .await
        .ok_or(Failure::Backend)?
        .map_err(|_| Failure::Backend)?;
    let value = snapshot
        .snapshot()
        .entries()
        .iter()
        .find(|entry| entry.producer().as_str() == rsi_agent_model_selection::VIEW)
        .and_then(|entry| entry.view())
        .ok_or(Failure::Backend)?;
    serde_json::from_value(value.value().clone()).map_err(|_| Failure::Backend)
}
fn model_value(model: &ModelRef) -> Result<String, Failure> {
    serde_json::to_string(model).map_err(|_| Failure::Backend)
}
async fn directory(
    models: &dyn LanguageModels,
) -> Result<Vec<schema::SessionConfigSelectOption>, Failure> {
    let mut choices = Vec::new();
    let mut after = None;
    loop {
        let page = models
            .list_models(after.as_ref(), MAX_LANGUAGE_MODEL_PAGE)
            .await
            .map_err(|_| Failure::Backend)?;
        if page.has_more && page.models.is_empty() {
            return Err(Failure::Backend);
        }
        for model in &page.models {
            if after.as_ref().is_some_and(|cursor| model <= cursor) {
                return Err(Failure::Backend);
            }
            let value = model_value(model)?;
            choices.push(schema::SessionConfigSelectOption::new(
                value,
                format!("{} / {}", model.deployment(), model.model()),
            ));
            after = Some(model.clone());
            if choices.len() > MAX_MODEL_CHOICES {
                return Err(Failure::Busy);
            }
        }
        if !page.has_more {
            break;
        }
        if choices.len() == MAX_MODEL_CHOICES {
            return Err(Failure::Busy);
        }
    }
    Ok(choices)
}

fn configuration(
    current: &ModelSelection,
    mut choices: Vec<schema::SessionConfigSelectOption>,
    efforts: Option<&ReasoningEffortProfile>,
) -> Result<Vec<schema::SessionConfigOption>, Failure> {
    struct FrameSize(usize);
    impl std::io::Write for FrameSize {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|size| *size <= 900_000)
                .ok_or_else(|| std::io::Error::other("configuration exceeds frame capacity"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let current_value = model_value(&current.model)?;
    if !choices
        .iter()
        .any(|choice| choice.value.0.as_ref() == current_value)
    {
        choices.insert(
            0,
            schema::SessionConfigSelectOption::new(
                current_value.clone(),
                format!("{} / {}", current.model.deployment(), current.model.model()),
            )
            .description("Current model is unavailable"),
        );
    }
    if choices.len() > MAX_MODEL_CHOICES {
        return Err(Failure::Busy);
    }
    // Exact escaped JSON size, without retaining another full response buffer.
    // Fixed setup fields and the result envelope use the remaining frame headroom.
    let option = schema::SessionConfigOption::select("model", "Model", current_value, choices)
        .category(schema::SessionConfigOptionCategory::Model);
    let mut options = vec![option];
    if let Some(efforts) = efforts.filter(|profile| !profile.supported().is_empty()) {
        let default = schema::SessionConfigSelectOption::new(DEFAULT_EFFORT, "Model default")
            .description(efforts.default_effort().map_or_else(
                || "Provider default is unknown".to_owned(),
                |effort| format!("Adapter default: {effort}"),
            ));
        let mut choices = vec![default];
        choices.extend(efforts.supported().iter().map(|effort| {
            schema::SessionConfigSelectOption::new(
                effort.as_str().to_owned(),
                effort.as_str().to_owned(),
            )
        }));
        if let Some(effort) = current
            .reasoning_effort
            .as_ref()
            .filter(|effort| !efforts.supported().contains(effort))
        {
            choices.push(
                schema::SessionConfigSelectOption::new(
                    effort.as_str().to_owned(),
                    effort.as_str().to_owned(),
                )
                .description("Current reasoning effort is unavailable"),
            );
        }
        options.push(
            schema::SessionConfigOption::select(
                "thought_level",
                "Reasoning effort",
                current
                    .reasoning_effort
                    .as_ref()
                    .map_or(DEFAULT_EFFORT, ReasoningEffortId::as_str)
                    .to_owned(),
                choices,
            )
            .category(schema::SessionConfigOptionCategory::ThoughtLevel),
        );
    }
    serde_json::to_writer(&mut FrameSize(0), &options).map_err(|_| Failure::Busy)?;
    Ok(options)
}

async fn described_efforts(
    model: &ModelRef,
    choices: &[schema::SessionConfigSelectOption],
    models: &dyn LanguageModels,
) -> Result<Option<ReasoningEffortProfile>, Failure> {
    let value = model_value(model)?;
    if !choices
        .iter()
        .any(|choice| choice.value.0.as_ref() == value)
    {
        return Ok(None);
    }
    let description = models
        .describe_model(model)
        .await
        .map_err(|_| Failure::Backend)?;
    Ok(Some(description.profile().reasoning_efforts().clone()))
}

pub(super) async fn options(
    session: &dyn SessionHandle,
    models: &dyn LanguageModels,
) -> Result<Vec<schema::SessionConfigOption>, Failure> {
    let current = selection(session).await?;
    let choices = directory(models).await?;
    let efforts = described_efforts(&current.model, &choices, models).await?;
    configuration(&current, choices, efforts.as_ref())
}
pub(super) async fn set(
    session: &dyn SessionHandle,
    models: &dyn LanguageModels,
    request: schema::SetSessionConfigOptionRequest,
) -> Result<Vec<schema::SessionConfigOption>, Failure> {
    if !matches!(request.config_id.0.as_ref(), "model" | "thought_level") {
        return Err(Failure::Parameters);
    }
    let schema::SessionConfigOptionValue::ValueId { value } = request.value else {
        return Err(Failure::Parameters);
    };
    let value = value.0.as_ref();
    let current = selection(session).await?;
    let choices = directory(models).await?;
    let selected = if request.config_id.0.as_ref() == "model" {
        if !choices
            .iter()
            .any(|choice| choice.value.0.as_ref() == value)
            && model_value(&current.model)? != value
        {
            return Err(Failure::Parameters);
        }
        ModelSelection {
            model: serde_json::from_str(value).map_err(|_| Failure::Parameters)?,
            reasoning_effort: None,
        }
    } else {
        ModelSelection {
            model: current.model.clone(),
            reasoning_effort: if value == DEFAULT_EFFORT {
                None
            } else {
                Some(ReasoningEffortId::new(value).map_err(|_| Failure::Parameters)?)
            },
        }
    };
    let description = models
        .describe_model(&selected.model)
        .await
        .map_err(|_| Failure::Parameters)?;
    let efforts = description.profile().reasoning_efforts();
    if request.config_id.0.as_ref() == "thought_level" && efforts.supported().is_empty() {
        return Err(Failure::Parameters);
    }
    efforts
        .resolve(selected.reasoning_effort.as_ref())
        .map_err(|_| Failure::Parameters)?;
    configuration(&selected, choices.clone(), Some(efforts))?;
    let arguments = CommandArguments::new(json!(selected)).map_err(|_| Failure::Parameters)?;
    let view = session.commands().await.map_err(|_| Failure::Backend)?;
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy).map_err(|_| Failure::Backend)?;
    let invocation = rsi_client::command_invocation(
        &view,
        rsi_agent_model_selection::COMMAND_NAME,
        arguments,
        DomainRequestId::new(format!("acp-{}", hex::encode(entropy)))
            .map_err(|_| Failure::Backend)?,
    )
    .map_err(|_| Failure::Parameters)?;
    rsi_client::execute_command_once(session, invocation)
        .await
        .map_err(|_| Failure::Backend)?;
    let actual = selection(session).await?;
    if actual.model == selected.model {
        return configuration(&actual, choices, Some(efforts));
    }
    let efforts = described_efforts(&actual.model, &choices, models).await?;
    configuration(&actual, choices, efforts.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rsi_ai_protocol::{LanguageModelPage, ModelsError};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct Catalog {
        count: usize,
        pages: AtomicUsize,
    }
    #[async_trait]
    impl LanguageModels for Catalog {
        async fn describe_model(
            &self,
            _: &ModelRef,
        ) -> Result<rsi_ai_protocol::LanguageModelDescription, ModelsError> {
            panic!("directory enumeration must not describe individual routes");
        }
        async fn list_models(
            &self,
            after: Option<&ModelRef>,
            limit: usize,
        ) -> Result<LanguageModelPage, ModelsError> {
            self.pages.fetch_add(1, Ordering::Relaxed);
            let start = after.map_or(0, |model| model.model().parse::<usize>().unwrap() + 1);
            let end = (start + limit).min(self.count);
            Ok(LanguageModelPage {
                models: (start..end)
                    .map(|n| ModelRef::new("fixture", format!("{n:05}")).unwrap())
                    .collect(),
                has_more: end < self.count,
            })
        }
    }

    #[derive(Debug)]
    struct Pages(Mutex<VecDeque<LanguageModelPage>>);
    #[async_trait]
    impl LanguageModels for Pages {
        async fn describe_model(
            &self,
            _: &ModelRef,
        ) -> Result<rsi_ai_protocol::LanguageModelDescription, ModelsError> {
            panic!("directory enumeration must not describe individual routes");
        }
        async fn list_models(
            &self,
            _: Option<&ModelRef>,
            _: usize,
        ) -> Result<LanguageModelPage, ModelsError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected catalog page"))
        }
    }

    #[tokio::test]
    async fn directory_requires_an_exclusive_cursor_and_distinguishes_deployments() {
        let models: Vec<_> = (0..MAX_LANGUAGE_MODEL_PAGE)
            .map(|n| ModelRef::new("a", format!("m{n:03}")).unwrap())
            .collect();
        let cursor = models.last().unwrap().clone();
        let pages = Pages(Mutex::new(VecDeque::from([
            LanguageModelPage {
                models,
                has_more: true,
            },
            LanguageModelPage {
                models: vec![cursor],
                has_more: true,
            },
        ])));
        assert!(matches!(directory(&pages).await, Err(Failure::Backend)));

        let a = ModelRef::new("a", "same").unwrap();
        let b = ModelRef::new("b", "same").unwrap();
        let pages = Pages(Mutex::new(VecDeque::from([LanguageModelPage {
            models: vec![a.clone(), b.clone()],
            has_more: false,
        }])));
        let choices = directory(&pages).await.unwrap();
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].value.0.as_ref(), model_value(&a).unwrap());
        assert_eq!(choices[1].value.0.as_ref(), model_value(&b).unwrap());
    }

    #[tokio::test]
    async fn directory_reads_only_bounded_pages_without_per_model_descriptions() {
        let models = Catalog {
            count: 4096,
            pages: AtomicUsize::new(0),
        };
        let choices = directory(&models).await.unwrap();
        assert_eq!(choices.len(), 4096);
        assert_eq!(models.pages.load(Ordering::Relaxed), 16);
        let available = ModelSelection {
            model: ModelRef::new("fixture", "00000").unwrap(),
            reasoning_effort: None,
        };
        assert!(configuration(&available, choices.clone(), None).is_ok());
        let unavailable = ModelSelection {
            model: ModelRef::new("gone", "model").unwrap(),
            reasoning_effort: None,
        };
        assert!(matches!(
            configuration(&unavailable, choices, None),
            Err(Failure::Busy)
        ));
        let models = Catalog {
            count: usize::MAX,
            pages: AtomicUsize::new(0),
        };
        assert!(matches!(directory(&models).await, Err(Failure::Busy)));
        assert_eq!(models.pages.load(Ordering::Relaxed), 16);
    }

    #[test]
    fn reasoning_choices_preserve_exact_ids_defaults_and_unavailable_selections() {
        let mut current = ModelSelection {
            model: ModelRef::new("fixture", "model").unwrap(),
            reasoning_effort: None,
        };
        let efforts = ReasoningEffortProfile::new(
            ["default", "high", "max"]
                .map(|value| ReasoningEffortId::new(value).unwrap())
                .into(),
            Some(ReasoningEffortId::new("high").unwrap()),
        )
        .unwrap();
        let values =
            serde_json::to_value(configuration(&current, vec![], Some(&efforts)).unwrap()).unwrap();
        assert_eq!(values[1]["category"], "thought_level");
        assert_eq!(values[1]["currentValue"], "@default");
        assert_eq!(
            values[1]["options"][0]["description"],
            "Adapter default: high"
        );
        assert_eq!(values[1]["options"][1]["value"], "default");
        assert_eq!(values[1]["options"][3]["value"], "max");
        current.reasoning_effort = Some(ReasoningEffortId::new("xhigh").unwrap());
        let values =
            serde_json::to_value(configuration(&current, vec![], Some(&efforts)).unwrap()).unwrap();
        assert_eq!(values[1]["currentValue"], "xhigh");
        assert_eq!(
            values[1]["options"][4]["description"],
            "Current reasoning effort is unavailable"
        );
        assert_eq!(configuration(&current, vec![], None).unwrap().len(), 1);
        assert_eq!(
            configuration(&current, vec![], Some(&ReasoningEffortProfile::default()))
                .unwrap()
                .len(),
            1
        );
        let unknown =
            ReasoningEffortProfile::new(vec![ReasoningEffortId::new("high").unwrap()], None)
                .unwrap();
        let values =
            serde_json::to_value(configuration(&current, vec![], Some(&unknown)).unwrap()).unwrap();
        assert_eq!(
            values[1]["options"][0]["description"],
            "Provider default is unknown"
        );
    }

    #[test]
    fn configuration_preserves_unavailable_selection_and_bounds_escaped_bytes() {
        let current = ModelSelection {
            model: ModelRef::new("gone", "model").unwrap(),
            reasoning_effort: None,
        };
        let value = serde_json::to_value(configuration(&current, vec![], None).unwrap()).unwrap();
        assert_eq!(
            value[0]["currentValue"],
            model_value(&current.model).unwrap()
        );
        assert_eq!(
            value[0]["options"][0]["description"],
            "Current model is unavailable"
        );
        // JSON escaping doubles each quote, so entry count or raw text size
        // would underestimate this otherwise bounded response.
        let choices: Vec<_> = (0..512)
            .map(|_| schema::SessionConfigSelectOption::new("q".repeat(500), "\"".repeat(700)))
            .collect();
        assert!(matches!(
            configuration(&current, choices, None),
            Err(Failure::Busy)
        ));
    }
}
