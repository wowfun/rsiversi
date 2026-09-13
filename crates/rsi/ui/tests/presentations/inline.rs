use super::*;

#[derive(Debug)]
pub(super) struct Inline(pub Arc<Source>);
impl BlockRenderer for Inline {
    fn render(&self, _: &Context, _: &BlockInput<'_>) -> Result<Option<UiView>> {
        Ok(None)
    }
    fn inline(
        &self,
        _: &Context,
        block: &BlockInput<'_>,
    ) -> Result<Option<Arc<dyn SurfaceRenderer>>> {
        Ok((block.key != "unrecognized").then(|| self.0.clone() as Arc<dyn SurfaceRenderer>))
    }
}

#[tokio::test]
async fn independent_inline_epochs_use_their_actual_source_and_existing_cleanup() {
    let (runtime, ui, source, fiber, panel) = setup().await;
    panel.close().await.unwrap();
    let target = runtime.root().lookup_local::<UiTargetContract>().unwrap();
    let sources = rsi_conversation::SourceIndex::default();
    let mut block = BlockInput {
        key: "first",
        text: "",
        tool: None,
        sources: &sources,
    };
    let first = ui.present_inline(&target, &block).unwrap().unwrap();
    block.key = "second";
    let second = ui.present_inline(&target, &block).unwrap().unwrap();
    source.gate.add_permits(2);
    let left = first.ready().await.unwrap();
    let right = second.ready().await.unwrap();
    assert_ne!(left.identity(), right.identity());
    assert!(
        second
            .invoke(&left.action("run").unwrap(), ActionInput::default())
            .await
            .is_err()
    );
    assert_eq!(source.mutations.load(Ordering::SeqCst), 0);
    assert_eq!(
        first
            .source(left.revision(), "raw", 1, 3)
            .await
            .unwrap()
            .as_bytes(),
        b"our"
    );
    first.close().await.unwrap();
    assert!(first.source(left.revision(), "raw", 0, 3).await.is_err());
    assert!(second.source(right.revision(), "raw", 0, 3).await.is_ok());
    block.key = "unrecognized";
    assert!(ui.present_inline(&target, &block).unwrap().is_none());
    assert!(fiber.dispose().await.is_clean());
    assert!(second.snapshot().is_err());
    second.close().await.unwrap();
    drop((left, right));
    assert_eq!(ui.presentation_usage(), (0, 0, 0));
    assert!(runtime.shutdown().await.is_clean());
}
