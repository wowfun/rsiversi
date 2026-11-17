use self::{jobs::ShellProducer, tool::registration};
use super::{ShellKind, ShellToolFactory};
use rsi_jobs::{JobProducerRegistration, Jobs, JobsContract};
use rsi_meta::{ActivationPlan, MetaError};
use rsi_process::{Process, ProcessContract};
use rsi_tools_protocol::ToolRegistrarContract;
use std::{ffi::OsString, path::PathBuf, sync::Arc};
mod jobs;
mod output;
mod tool;

pub(super) fn activate_producer(mut plan: ActivationPlan, kind: ShellKind) -> rsi_meta::Result<()> {
    let (): () = plan.take_state()?;
    let jobs = plan.local::<JobsContract>()?;
    let process = plan.local::<ProcessContract>()?;
    let lease = jobs
        .register_producer(JobProducerRegistration {
            name: kind.producer().into(),
            producer: Arc::new(ShellProducer { process }),
        })
        .map_err(|error| MetaError::Activation(error.to_string()))?;
    plan.defer(
        "retire shell Job producer",
        Box::new(move || {
            Box::pin(async move { lease.retire().await.map_err(|error| error.to_string()) })
        }),
    )
}

pub(super) fn activate_tool(
    mut plan: ActivationPlan,
    factory: &ShellToolFactory,
) -> rsi_meta::Result<()> {
    let (): () = plan.take_state()?;
    let registrar = plan.local::<ToolRegistrarContract>()?;
    let jobs = plan.local::<JobsContract>()?;
    let process = plan.local::<ProcessContract>()?;
    let services = Arc::new(ShellServices {
        spill: plan
            .context()
            .lookup_local::<rsi_spill::SpillStoreContract>(),
        kind: factory.kind,
        program: factory.program.clone(),
        environment: factory.environment.clone(),
        process,
        jobs,
    });
    let registration = registration(services).map_err(|error| {
        MetaError::Activation(format!("invalid shell tool definition: {error}"))
    })?;
    let lease = registrar
        .register_batch(vec![registration])
        .map_err(|error| MetaError::Activation(error.to_string()))?;
    plan.defer(
        "release shell Tool contribution lease",
        Box::new(move || {
            Box::pin(async move { lease.retire().map_err(|error| error.to_string()) })
        }),
    )
}

#[derive(Debug)]
struct ShellServices {
    spill: Option<Arc<dyn rsi_spill::SpillStore>>,
    kind: ShellKind,
    program: PathBuf,
    environment: Arc<[(OsString, OsString)]>,
    process: Arc<dyn Process>,
    jobs: Arc<dyn Jobs>,
}
