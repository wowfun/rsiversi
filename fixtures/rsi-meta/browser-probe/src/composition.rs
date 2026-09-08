use super::*;
use rsi_meta::{Emit, EmitEventHandler, LocalEvent, LocalEventOptions};
use rsi_meta_scope::{ScopeRoot, ScopedContributions};
use std::convert::Infallible;

struct Notice;
impl LocalEvent for Notice {
    const KEY: &'static str = "probe.order";
    type Value = ();
    type Error = Infallible;
    type Mode = Emit;
}

#[derive(Debug)]
struct Record(String, Arc<Mutex<Vec<String>>>);
impl EmitEventHandler<Notice> for Record {
    fn handle(&self, _: &()) {
        self.1.lock().unwrap().push(self.0.clone());
    }
}

#[derive(Debug)]
struct Contribute {
    name: &'static str,
    events: Arc<Mutex<Vec<String>>>,
    contributions: Arc<ScopedContributions<String>>,
}
#[async_trait]
impl PluginFactory for Contribute {
    fn prepare(&self, config: &serde_json::Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        for (prepend, suffix) in [(false, "a"), (true, "p")] {
            plan.context().on_emit::<Notice, _>(
                Arc::new(Record(
                    format!("{}{suffix}", self.name),
                    self.events.clone(),
                )),
                LocalEventOptions {
                    prepend,
                    once: false,
                },
            )?;
        }
        let lease = self
            .contributions
            .register(
                &plan.context().registration_context()?,
                None,
                Arc::new(self.name.to_owned()),
            )
            .unwrap();
        plan.defer(
            "release contribution lease",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )
    }
}

pub(super) async fn probe(execution: Execution) {
    let runtime = Runtime::with_execution(Default::default(), execution).unwrap();
    let root = runtime.root();
    let scope = ScopeRoot::new(16).unwrap();
    let table = Arc::new(ScopedContributions::new(runtime.identity(), scope, 4).unwrap());
    let events = Arc::new(Mutex::new(Vec::new()));
    let a = root.child_position().unwrap();
    let b = root.child_position().unwrap();
    let factory = |name| {
        resolved(
            "ordered",
            Contribute {
                name,
                events: events.clone(),
                contributions: table.clone(),
            },
        )
    };
    let second = root
        .with_child_position(&b)
        .unwrap()
        .apply(factory("b"), serde_json::Value::Null)
        .await
        .unwrap();
    let first = root
        .with_child_position(&a)
        .unwrap()
        .apply(factory("a"), serde_json::Value::Null)
        .await
        .unwrap();
    let dispatch = || {
        root.dispatch_local::<Notice>(()).unwrap();
        std::mem::take(&mut *events.lock().unwrap())
    };
    assert_eq!(dispatch(), ["bp", "ap", "aa", "ba"]);
    let original = second.snapshot().generation;
    let before = table.snapshot(None).unwrap();
    assert_eq!(
        before.iter().map(|v| v.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(Arc::ptr_eq(&before, &table.snapshot(None).unwrap()));
    root.reorder_children(&[b.clone(), a.clone()]).unwrap();
    assert_eq!(dispatch(), ["ap", "bp", "ba", "aa"]);
    assert_eq!(
        table
            .snapshot(None)
            .unwrap()
            .iter()
            .map(|v| v.as_str())
            .collect::<Vec<_>>(),
        ["b", "a"]
    );
    assert!(first.dispose().await.is_clean());
    root.with_child_position(&a)
        .unwrap()
        .apply(factory("a2"), serde_json::Value::Null)
        .await
        .unwrap();
    root.reorder_children(&[a, b]).unwrap();
    assert_eq!(dispatch(), ["bp", "a2p", "a2a", "ba"]);
    assert_eq!(second.snapshot().generation, original);
    assert_eq!(
        table
            .snapshot(None)
            .unwrap()
            .iter()
            .map(|v| v.as_str())
            .collect::<Vec<_>>(),
        ["a2", "b"]
    );
    assert_eq!(
        before.iter().map(|v| v.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(runtime.shutdown().await.is_clean());
    assert!(table.snapshot(None).unwrap().is_empty());
    assert_eq!(runtime.resource_snapshot().effects.current, 0);
    assert_eq!(runtime.resource_snapshot().listeners.current, 0);
}
