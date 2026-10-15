//! Owned synchronous pipe operations, native cancellation and actual joins.
use super::native;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read as _, Write as _},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Debug)]
struct Operation {
    cancel: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
    control: native::ThreadControl,
}
impl Operation {
    fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        self.control.cancel();
    }
    fn finished(&self) -> bool {
        self.thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_none_or(JoinHandle::is_finished)
    }
    async fn join(&self) -> bool {
        while !self.finished() {
            if self.cancel.load(Ordering::Acquire) {
                self.control.cancel();
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        self.thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .is_none_or(|thread| thread.join().is_ok())
    }
}

type Receipt = std::io::Result<Vec<u8>>;
#[derive(Debug)]
pub(super) struct Pipe {
    file: Arc<File>,
    input: bool,
    slot: Arc<Semaphore>,
    buffered: Arc<Mutex<Option<Receipt>>>,
    active: Mutex<BTreeMap<u64, Arc<Operation>>>,
    serial: AtomicU64,
    stop: CancellationToken,
    cleanup: TaskTracker,
    write_serial: tokio::sync::Mutex<()>,
}
impl Pipe {
    pub(super) fn new(file: File, input: bool) -> Arc<Self> {
        Arc::new(Self {
            file: Arc::new(file),
            input,
            slot: Arc::new(Semaphore::new(1)),
            buffered: Arc::new(Mutex::new(None)),
            active: Mutex::new(BTreeMap::new()),
            serial: AtomicU64::new(0),
            stop: CancellationToken::new(),
            cleanup: TaskTracker::new(),
            write_serial: tokio::sync::Mutex::new(()),
        })
    }
    fn start(
        self: &Arc<Self>,
        bytes: Option<Vec<u8>>,
        permit: OwnedSemaphorePermit,
    ) -> std::io::Result<(u64, Arc<Operation>)> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stop.is_cancelled() {
            return Err(std::io::Error::other("stdio is closed"));
        }
        let id = self
            .serial
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| std::io::Error::other("stdio operation identity exhausted"))?;
        let file = self.file.clone();
        let buffered = self.buffered.clone();
        let (send, receive) = std::sync::mpsc::sync_channel::<Arc<Operation>>(1);
        let thread = std::thread::Builder::new()
            .name("rsi-acp-pipe".into())
            .spawn(move || {
                let Ok(operation) = receive.recv() else {
                    return;
                };
                let _permit = permit;
                if operation.cancel.load(Ordering::Acquire) {
                    return;
                }
                let result = if let Some(bytes) = bytes {
                    (&*file).write_all(&bytes).map(|()| vec![])
                } else {
                    let mut bytes = vec![0; 65536];
                    (&*file).read(&mut bytes).map(|size| {
                        bytes.truncate(size);
                        bytes
                    })
                };
                if !operation.cancel.load(Ordering::Acquire) || result.is_ok() {
                    *buffered
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                }
            })?;
        let control = match native::ThreadControl::new(&thread) {
            Ok(control) => control,
            Err(error) => {
                drop(send);
                let _ = thread.join();
                return Err(error);
            }
        };
        let operation = Arc::new(Operation {
            cancel: AtomicBool::new(false),
            thread: Mutex::new(Some(thread)),
            control,
        });
        active.insert(id, operation.clone());
        if send.send(operation.clone()).is_err() {
            operation.cancel();
            return Err(std::io::Error::other(
                "stdio worker stopped during admission",
            ));
        }
        Ok((id, operation))
    }
    async fn execute(self: &Arc<Self>, bytes: Option<Vec<u8>>) -> Result<Vec<u8>, rsi_acp::Error> {
        let permit = tokio::select! {
            () = self.stop.cancelled() => return Err(rsi_acp::Error::Closed),
            permit = self.slot.clone().acquire_owned() => permit.map_err(|_| rsi_acp::Error::Closed)?,
        };
        if self.stop.is_cancelled() {
            return Err(rsi_acp::Error::Closed);
        }
        if self.input
            && let Some(result) = self
                .buffered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        {
            return result.map_err(|_| rsi_acp::Error::Closed);
        }
        let (id, operation) = self
            .start(bytes, permit)
            .map_err(|_| rsi_acp::Error::Closed)?;
        let mut guard = Guard {
            pipe: self.clone(),
            operation: operation.clone(),
            id,
            armed: true,
        };
        tokio::select! {
            () = self.stop.cancelled() => return Err(rsi_acp::Error::Closed),
            clean = operation.join() => { if !clean { return Err(rsi_acp::Error::Closed); } }
        }
        guard.armed = false;
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        self.buffered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(rsi_acp::Error::Closed)?
            .map_err(|_| rsi_acp::Error::Closed)
    }
    pub(super) async fn read(self: &Arc<Self>) -> Result<Vec<u8>, rsi_acp::Error> {
        self.execute(None).await
    }
    pub(super) async fn write(self: &Arc<Self>, bytes: &[u8]) -> Result<(), rsi_acp::Error> {
        let mut frame = FrameGuard {
            pipe: self.clone(),
            armed: true,
        };
        let _write = self.write_serial.lock().await;
        for chunk in bytes.chunks(65536) {
            self.execute(Some(chunk.to_vec())).await?;
        }
        frame.armed = false;
        Ok(())
    }
    pub(super) async fn close(self: &Arc<Self>) -> Result<(), rsi_acp::Error> {
        self.stop.cancel();
        let operations = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for operation in &operations {
            operation.cancel();
        }
        let mut clean = true;
        for operation in operations {
            clean &= operation.join().await;
        }
        self.cleanup.close();
        self.cleanup.wait().await;
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        if clean {
            Ok(())
        } else {
            Err(rsi_acp::Error::Closed)
        }
    }
}
struct FrameGuard {
    pipe: Arc<Pipe>,
    armed: bool,
}
impl Drop for FrameGuard {
    fn drop(&mut self) {
        if self.armed {
            self.pipe.stop.cancel();
        }
    }
}
struct Guard {
    pipe: Arc<Pipe>,
    operation: Arc<Operation>,
    id: u64,
    armed: bool,
}
impl Drop for Guard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.operation.cancel();
        if !self.pipe.input {
            self.pipe.stop.cancel();
        }
        let pipe = self.pipe.clone();
        let operation = self.operation.clone();
        let id = self.id;
        self.pipe.cleanup.spawn(async move {
            operation.join().await;
            pipe.active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&id);
        });
    }
}
