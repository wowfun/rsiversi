use super::output::map_process;
use async_trait::async_trait;
use rsi_jobs::{
    JobControl, JobOutputRead, JobProducer, JobRequest, JobStatus, JobStream, JobTerminal,
    JobsError,
};
use rsi_process::{ManagedProcess, Process, ProcessSpec};
use rsi_tools_protocol::ToolProcess;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug)]
pub(super) struct ShellJobRequest {
    pub(super) spec: Mutex<Option<ProcessSpec<ToolProcess>>>,
}

#[derive(Debug)]
pub(super) struct ShellProducer {
    pub(super) process: Arc<dyn Process>,
}

#[async_trait::async_trait]

impl JobProducer for ShellProducer {
    async fn start(&self, request: &JobRequest) -> rsi_jobs::Result<Arc<dyn JobControl>> {
        let request = request.downcast_ref::<ShellJobRequest>().ok_or_else(|| {
            JobsError::InvalidInput("shell producer received the wrong request type".into())
        })?;
        let spec = request
            .spec
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                JobsError::InvalidInput("shell prepared request was already consumed".into())
            })?;
        let process = ToolProcess::spawn(spec, self.process.as_ref())
            .await
            .map_err(map_process)?;
        Ok(Arc::new(ShellJobControl {
            process,
            cancellation_requested: AtomicBool::new(false),
        }))
    }
}

#[derive(Debug)]
struct ShellJobControl {
    process: ManagedProcess,
    cancellation_requested: AtomicBool,
}

#[async_trait]
impl JobControl for ShellJobControl {
    fn peek(&self, stream: JobStream, maximum: usize) -> rsi_jobs::Result<Option<JobOutputRead>> {
        let reader = match stream {
            JobStream::Stdout => self.process.stdout(),
            JobStream::Stderr => self.process.stderr(),
        };
        let read = reader.peek_tail(maximum).map_err(map_process)?;
        Ok(Some(JobOutputRead {
            full_output: read.full_output,
            bytes: read.bytes,
            oldest_offset: read.oldest_offset,
            next_offset: read.next_offset,
            lossy: read.lossy,
        }))
    }
    fn read(&self, stream: JobStream, offset: u64) -> rsi_jobs::Result<JobOutputRead> {
        let read = match stream {
            JobStream::Stdout => self.process.stdout().read_from(offset),
            JobStream::Stderr => self.process.stderr().read_from(offset),
        }
        .map_err(map_process)?;
        Ok(JobOutputRead {
            full_output: read.full_output,
            bytes: read.bytes,
            oldest_offset: read.oldest_offset,
            next_offset: read.next_offset,
            lossy: read.lossy,
        })
    }

    fn cancel(&self) {
        self.cancellation_requested.store(true, Ordering::Release);
        self.process.terminate();
    }

    async fn wait(&self) -> rsi_jobs::Result<JobTerminal> {
        let outcome = self.process.wait().await.map_err(map_process)?;
        let cancellation_requested = self.cancellation_requested.load(Ordering::Acquire);
        Ok(JobTerminal {
            status: if cancellation_requested {
                JobStatus::Cancelled
            } else if outcome.signal.is_none()
                && outcome.exit_code.is_none_or(|exit_code| exit_code == 0)
            {
                JobStatus::Completed
            } else {
                JobStatus::Failed
            },
            exit_code: outcome.exit_code,
            signal: outcome.signal,
            message: None,
        })
    }
}
