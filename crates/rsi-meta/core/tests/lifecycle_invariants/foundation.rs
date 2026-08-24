use super::*;

#[derive(Debug)]
struct EffectFactory {
    descriptor: PluginDescriptor,
    log: Arc<Mutex<Vec<String>>>,
    fail: bool,
}

#[async_trait]
impl PluginFactory for EffectFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, config: Arc<Value>) -> Result<()> {
        let generation = context.owner().expect("plugin has owner").1.0;
        self.log
            .lock()
            .expect("effect log poisoned")
            .push(format!("activate:{generation}:{config}"));
        for label in ["a", "b"] {
            let log = Arc::clone(&self.log);
            context.defer(
                label,
                Box::new(move || {
                    async move {
                        log.lock()
                            .expect("effect log poisoned")
                            .push(format!("cleanup:{label}"));
                        Ok(())
                    }
                    .boxed()
                }),
            )?;
        }
        if self.fail {
            return Err(MetaError::Activation("requested failure".to_owned()));
        }
        Ok(())
    }
}

#[tokio::test]
async fn failed_setup_and_reconfigure_cleanup_in_strict_reverse_order() {
    let runtime = Runtime::default();
    let failed_log = Arc::new(Mutex::new(Vec::new()));
    let failed = runtime
        .root()
        .apply(
            Arc::new(EffectFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("failed", "1")),
                log: Arc::clone(&failed_log),
                fail: true,
            }),
            json!(1),
        )
        .await
        .unwrap();
    assert!(matches!(failed.snapshot().state, FiberState::Failed(_)));
    assert_eq!(
        failed_log.lock().expect("effect log poisoned").as_slice(),
        &["activate:1:1", "cleanup:b", "cleanup:a"]
    );

    let log = Arc::new(Mutex::new(Vec::new()));
    let active = runtime
        .root()
        .apply(
            Arc::new(EffectFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("effect", "1")),
                log: Arc::clone(&log),
                fail: false,
            }),
            json!(1),
        )
        .await
        .unwrap();
    let first_generation = active.snapshot().generation;
    active.reconfigure(json!(2)).await.unwrap();
    assert!(active.snapshot().generation > first_generation);
    assert_eq!(
        log.lock().expect("effect log poisoned").as_slice(),
        &["activate:2:1", "cleanup:b", "cleanup:a", "activate:3:2"]
    );
    active.dispose().await;
    assert_eq!(
        &log.lock().expect("effect log poisoned")[3..],
        &["activate:3:2", "cleanup:b", "cleanup:a"]
    );
}

#[derive(Debug)]
struct ParentFactory {
    descriptor: PluginDescriptor,
    child: Arc<dyn PluginFactory>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl PluginFactory for ParentFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.apply(Arc::clone(&self.child), Value::Null).await?;
        let log = Arc::clone(&self.log);
        context.defer(
            "parent",
            Box::new(move || {
                async move {
                    log.lock().expect("child log poisoned").push("parent");
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[derive(Debug)]
struct ChildFactory {
    descriptor: PluginDescriptor,
    log: Arc<Mutex<Vec<&'static str>>>,
}

#[derive(Debug)]
struct NamedChildFactory {
    descriptor: PluginDescriptor,
    label: &'static str,
    log: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl PluginFactory for NamedChildFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let label = self.label;
        let log = Arc::clone(&self.log);
        context.defer(
            label,
            Box::new(move || {
                async move {
                    log.lock().expect("child log poisoned").push(label);
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[derive(Debug)]
struct MultiChildParentFactory {
    descriptor: PluginDescriptor,
    log: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl PluginFactory for MultiChildParentFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        for label in ["first-child", "second-child"] {
            context
                .apply(
                    Arc::new(NamedChildFactory {
                        descriptor: PluginDescriptor::new(FactoryIdentity::builtin(label, "1")),
                        label,
                        log: Arc::clone(&self.log),
                    }),
                    Value::Null,
                )
                .await?;
        }
        let log = Arc::clone(&self.log);
        context.defer(
            "parent",
            Box::new(move || {
                async move {
                    log.lock().expect("parent log poisoned").push("parent");
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[async_trait]
impl PluginFactory for ChildFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let log = Arc::clone(&self.log);
        context.defer(
            "child",
            Box::new(move || {
                async move {
                    log.lock().expect("child log poisoned").push("child");
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test]
async fn parent_disposes_children_before_its_own_effects_and_dispose_is_joinable() {
    let runtime = Runtime::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let parent = runtime
        .root()
        .apply(
            Arc::new(ParentFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("parent", "1")),
                child: Arc::new(ChildFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin("child", "1")),
                    log: Arc::clone(&log),
                }),
                log: Arc::clone(&log),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&parent).await;
    let (left, right) = tokio::join!(parent.dispose(), parent.dispose());
    assert!(left.is_clean());
    assert!(right.is_clean());
    assert_eq!(
        log.lock().expect("child log poisoned").as_slice(),
        &["child", "parent"]
    );
}

#[tokio::test]
async fn parent_disposes_multiple_children_in_reverse_application_order() {
    let runtime = Runtime::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let parent = runtime
        .root()
        .apply(
            Arc::new(MultiChildParentFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "multi-child-parent",
                    "1",
                )),
                log: Arc::clone(&log),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&parent).await;

    assert!(parent.dispose().await.is_clean());
    assert_eq!(
        log.lock().expect("cleanup log poisoned").as_slice(),
        &["second-child", "first-child", "parent"]
    );
}

#[tokio::test]
async fn parent_reconfiguration_retires_the_old_child_before_publishing_a_new_generation() {
    let runtime = Runtime::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let parent = runtime
        .root()
        .apply(
            Arc::new(ParentFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "reconfigured-parent",
                    "1",
                )),
                child: Arc::new(ChildFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "reconfigured-child",
                        "1",
                    )),
                    log: Arc::clone(&log),
                }),
                log: Arc::clone(&log),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&parent).await;
    let first_generation = parent.snapshot().generation;

    parent.reconfigure(Value::Null).await.unwrap();

    assert!(parent.snapshot().generation > first_generation);
    assert_eq!(
        log.lock().expect("cleanup log poisoned").as_slice(),
        &["child", "parent"]
    );
    assert_eq!(runtime.snapshot().fibers.len(), 2);
    assert!(parent.dispose().await.is_clean());
    assert_eq!(
        log.lock().expect("cleanup log poisoned").as_slice(),
        &["child", "parent", "child", "parent"]
    );
}
