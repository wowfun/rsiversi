use super::*;
fn locator(byte: &str) -> rsi_spill::SpillLocator {
    rsi_spill::SpillLocator::parse(format!("{}-{}", "0".repeat(64), byte.repeat(32))).unwrap()
}
use serde_json::{Value, json};
use sources::view;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Debug, Default)]
struct Cache {
    reads: Mutex<Vec<(String, u64)>>,
    block: AtomicBool,
    unavailable: AtomicBool,
    active: AtomicUsize,
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl rsi_spill::SpillReader for Cache {
    async fn read(
        &self,
        id: &rsi_spill::SpillLocator,
        offset: u64,
        limit: usize,
    ) -> rsi_spill::Result<rsi_spill::SpillPage> {
        self.reads.lock().unwrap().push((id.to_string(), offset));
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = Active(&self.active);
        if self.block.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(rsi_spill::SpillError::Unavailable(
                "completed output is unavailable".into(),
            ));
        }
        let mut bytes = match id.object_name().as_bytes()[0] {
            b'a' => b"STDOUT\n".to_vec(),
            b'b' => b"STDERR\n".to_vec(),
            b'c' => b"OUTPUT\n".to_vec(),
            _ => panic!("unexpected fixture locator"),
        };
        bytes.extend_from_slice(b"<script>literal</script>\x00\x1b\xff\n");
        bytes.resize(16 * 1024, b'x');
        bytes.extend_from_slice(b"SECOND-PAGE\x00\xff\n");
        let start = usize::try_from(offset).unwrap();
        let end = (start + limit).min(bytes.len());
        let page = rsi_spill::SpillPage {
            locator: id.clone(),
            offset,
            next_offset: end as u64,
            total_bytes: bytes.len() as u64,
            bytes: bytes[start..end].to_vec().into(),
        };
        page.validate_for(id, offset, limit).unwrap();
        Ok(page)
    }
}
#[derive(Debug)]
struct Supply(Arc<Cache>);
#[async_trait]
impl PluginFactory for Supply {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<rsi_spill::SpillReaderContract>(self.0.clone())?;
        plan.defer(
            "withdraw output cache",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
async fn idle(cache: &Cache, active: bool) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while (cache.active.load(Ordering::SeqCst) > 0) != active {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one output view read, paging, close and replacement authority sequence"
)]
async fn output_cards_preserve_raw_stream_pages_and_fence_closed_or_replaced_views() {
    let (runtime, backend, app) = sources::fixture().await;
    let cache = Arc::new(Cache::default());
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "output",
                "fixture",
                UpdateMode::Replayable,
                Arc::new(Supply(cache.clone())),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(fiber.snapshot().state, FiberState::Active);
    let mut result = rsi_tools_protocol::ToolResult::new(json!({"exit_code":7,"stdout":{"full_output":locator("a").to_string()},"stderr":{"full_output":locator("b").to_string()}}),vec![],false).unwrap();
    result.spills = ["a", "b", "c"]
        .map(|id| rsi_spill::SpillRef {
            locator: locator(id),
            bytes: 16_398,
        })
        .into();
    backend.facts.lock().unwrap().push(
        SessionFact::new(
            9,
            1,
            SessionFactBody::ToolResult {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("effect").unwrap(),
                identity: rsi_tools_protocol::ToolResultIdentity::new(
                    "owner",
                    "invocation",
                    "call",
                    "a".repeat(64),
                )
                .unwrap(),
                result,
                conclusion: None,
            },
        )
        .unwrap(),
    );
    let session = view(&app)["surfaces"]["main"]["session"].clone();
    app.command(&json!({"action":"open","pane":"main","session":session}).to_string())
        .await
        .unwrap();
    let current = view(&app);
    let pane = &current["surfaces"]["main"];
    let open = json!({"action":"ui_block","pane":"main","generation":pane["generation"],"key":pane["transcript"]["blocks"][0]["key"]}).to_string();
    for (label, id, prefix) in [
        ("Read stdout", "a", "STDOUT"),
        ("Read stderr", "b", "STDERR"),
        ("Read output 1", "c", "OUTPUT"),
    ] {
        app.command(&open).await.unwrap();
        let card = super::sources::panel(&app)["ui_detail"].clone();
        assert!(card.to_string().contains("command failed"));
        assert!(
            !card.to_string().contains("Read output 2"),
            "stream references must not produce duplicate generic buttons"
        );
        let read = ui::button(&card, Some(label));
        app.command(&read.to_string()).await.unwrap();
        let first = super::sources::panel(&app)["ui_detail"].clone();
        let code = first["model"]["standard_view"]["elements"][2]["text"]
            .as_str()
            .unwrap();
        assert!(code.starts_with(prefix));
        assert!(code.contains("<script>literal</script>���"));
        assert!(!code.contains('\u{1b}'));
        assert_eq!(
            cache.reads.lock().unwrap().last().unwrap(),
            &(locator(id).to_string(), 0)
        );
        let next = ui::button(&first, Some("Next page"));
        app.command(&next.to_string()).await.unwrap();
        assert!(
            super::sources::panel(&app)["ui_detail"]
                .to_string()
                .contains("SECOND-PAGE")
        );
        let hex = ui::button(
            &super::sources::panel(&app)["ui_detail"],
            Some("View exact hex"),
        );
        app.command(&hex.to_string()).await.unwrap();
        assert!(
            super::sources::panel(&app)["ui_detail"]
                .to_string()
                .contains("00004000  53 45 43")
        );
        assert!(
            super::sources::panel(&app)["ui_detail"]
                .to_string()
                .contains("00 ff")
        );
        let reads = cache.reads.lock().unwrap().len();
        assert!(app.command(&next.to_string()).await.is_err());
        assert_eq!(cache.reads.lock().unwrap().len(), reads);
        cache.unavailable.store(true, Ordering::SeqCst);
        let previous = ui::button(
            &super::sources::panel(&app)["ui_detail"],
            Some("Previous page"),
        );
        app.command(&previous.to_string()).await.unwrap();
        assert!(
            super::sources::panel(&app)["ui_detail"]["error"]
                .as_str()
                .unwrap()
                .contains("unavailable")
        );
        cache.unavailable.store(false, Ordering::SeqCst);
    }
    for close in [true, false] {
        app.command(&open).await.unwrap();
        let read = ui::button(
            &super::sources::panel(&app)["ui_detail"],
            Some("Read stdout"),
        );
        cache.block.store(true, Ordering::SeqCst);
        let closing_view = super::sources::panel(&app)["view"].clone();
        let pending = app.command(&read.to_string());
        idle(&cache, true).await;
        if close {
            super::sources::close_panel(&app).await;
        } else {
            app.command(&json!({"action":"open","pane":"main","session":session}).to_string())
                .await
                .unwrap();
        }
        idle(&cache, false).await;
        pending.await.unwrap();
        assert!(
            !view(&app)["panels"]
                .as_array()
                .unwrap()
                .iter()
                .any(|panel| panel["view"] == closing_view)
        );
        cache.block.store(false, Ordering::SeqCst);
    }
    assert!(backend.cancel.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}
