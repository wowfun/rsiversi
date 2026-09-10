use crate::server::Body;
use bytes::Bytes;
use futures_util::task::AtomicWaker;
use http_body_util::BodyExt;
use rsi_api_protocol::ApiAdmission;
use std::{
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{OwnedSemaphorePermit, Semaphore, watch},
};

// Admission and connection activity survive codec buffering together. The permit
// drops first so the last active stream can transfer it back to idle ownership.
struct Retained {
    admission: Admission,
    activity: Option<ActivityLease>,
}
enum Admission {
    Handshake { _permit: OwnedSemaphorePermit },
    Operation { _lease: ApiAdmission },
}
pub(crate) struct Delivery {
    admission: Mutex<Option<Retained>>,
    flush: Option<Arc<Flush>>,
}
impl Delivery {
    pub fn new(permit: OwnedSemaphorePermit, flush: Option<Arc<Flush>>) -> Arc<Self> {
        Arc::new(Self {
            admission: Mutex::new(Some(Retained {
                admission: Admission::Handshake { _permit: permit },
                activity: None,
            })),
            flush,
        })
    }
    pub fn for_h2(activity: &Arc<Activity>, flush: Arc<Flush>) -> Option<Arc<Self>> {
        let (permit, lease) = activity.start()?;
        Some(Arc::new(Self {
            admission: Mutex::new(Some(Retained {
                admission: Admission::Handshake { _permit: permit },
                activity: Some(lease),
            })),
            flush: Some(flush),
        }))
    }
    pub fn admit(&self, admission: ApiAdmission) {
        if let Some(retained) = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            retained.admission = Admission::Operation { _lease: admission };
        }
    }
    pub fn begin_body(&self, streaming: bool) {
        self.update_activity(|stream| {
            stream.streaming = streaming;
            stream.deadline =
                Some(tokio::time::Instant::now() + std::time::Duration::from_secs(30));
        });
    }
    fn update_activity(&self, change: impl FnOnce(&mut StreamDelivery)) {
        let retained = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(lease) = retained.as_ref().and_then(|r| r.activity.as_ref()) {
            lease.update(change);
        }
    }
    pub fn body(self: &Arc<Self>, body: Body) -> Body {
        let delivery = self.clone();
        body.map_frame(move |frame| {
            frame.map_data(|bytes| {
                delivery.update_activity(|stream| {
                    stream.frames += 1;
                    stream.saw_frame = true;
                    stream.deadline.get_or_insert_with(|| {
                        tokio::time::Instant::now() + std::time::Duration::from_secs(30)
                    });
                });
                Bytes::from_owner(FrameOwner {
                    bytes,
                    delivery: delivery.clone(),
                })
            })
        })
        .boxed_unsync()
    }
}
impl Drop for Delivery {
    fn drop(&mut self) {
        let admission = self
            .admission
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let (Some(flush), Some(admission)) = (&self.flush, admission) {
            flush.defer(admission);
        }
    }
}
struct FrameOwner {
    bytes: Bytes,
    delivery: Arc<Delivery>,
}
impl Drop for FrameOwner {
    fn drop(&mut self) {
        self.delivery.update_activity(|stream| stream.frames -= 1);
    }
}
impl AsRef<[u8]> for FrameOwner {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Default)]
pub(crate) struct Flush {
    state: Mutex<FlushState>,
    changed: AtomicWaker,
    activity: Mutex<std::sync::Weak<Activity>>,
}
#[derive(Default)]
struct FlushState {
    closed: bool,
    pending: Vec<Retained>,
}
impl Flush {
    fn defer(&self, admission: Retained) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.closed {
            // Every element retains a previously acquired global admission or
            // unclassified slot. The queue cannot exceed those existing bounds.
            state.pending.push(admission);
            drop(state);
            self.changed.wake();
        }
    }
    fn release(&self, close: bool) {
        let pending = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed |= close;
            std::mem::take(&mut state.pending)
        };
        drop(pending);
        if !close
            && let Some(activity) = self
                .activity
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .upgrade()
        {
            activity.flushed();
        }
    }
}

pub(crate) struct FlushIo<T> {
    inner: T,
    flush: Arc<Flush>,
}
impl<T> FlushIo<T> {
    pub fn new(inner: T, flush: Arc<Flush>) -> Self {
        Self { inner, flush }
    }
}
impl<T> Drop for FlushIo<T> {
    fn drop(&mut self) {
        self.flush.release(true);
    }
}
impl<T: AsyncRead + Unpin> AsyncRead for FlushIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.flush.changed.register(cx.waker());
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for FlushIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, bytes)
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, buffers)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_flush(cx);
        if result.is_ready() {
            self.flush.release(matches!(result, Poll::Ready(Err(_))));
        }
        result
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_shutdown(cx);
        if result.is_ready() {
            self.flush.release(true);
        }
        result
    }
}

/// Connection activity ends only after its last response has left codec/transport
/// retention. Idle sockets remain under the same global unclassified bound.
pub(crate) struct Activity {
    slots: Arc<Semaphore>,
    state: Mutex<ActivityState>,
    idle: watch::Sender<Option<tokio::time::Instant>>,
    closed: tokio_util::sync::CancellationToken,
}
struct ActivityState {
    active: std::collections::BTreeMap<u64, StreamDelivery>,
    next: u64,
    idle: Option<OwnedSemaphorePermit>,
    idle_deadline: Option<tokio::time::Instant>,
}
#[derive(Default)]
struct StreamDelivery {
    streaming: bool,
    saw_frame: bool,
    frames: usize,
    deadline: Option<tokio::time::Instant>,
}
struct ActivityLease {
    activity: Arc<Activity>,
    id: u64,
}
impl Activity {
    pub fn new(slots: Arc<Semaphore>, permit: OwnedSemaphorePermit) -> Arc<Self> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        Arc::new(Self {
            slots,
            state: Mutex::new(ActivityState {
                active: std::collections::BTreeMap::new(),
                next: 0,
                idle: Some(permit),
                idle_deadline: Some(deadline),
            }),
            idle: watch::channel(Some(deadline)).0,
            closed: tokio_util::sync::CancellationToken::new(),
        })
    }
    pub fn track_flush(self: &Arc<Self>, flush: &Flush) {
        *flush
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::downgrade(self);
    }
    fn publish_deadline(&self, state: &ActivityState) {
        self.idle.send_replace(
            state
                .active
                .values()
                .filter_map(|stream| stream.deadline)
                .chain(state.idle_deadline)
                .min(),
        );
    }
    fn flushed(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for stream in state.active.values_mut() {
            if stream.streaming && stream.saw_frame && stream.frames == 0 {
                stream.deadline = None;
            }
        }
        self.publish_deadline(&state);
    }
    fn start(self: &Arc<Self>) -> Option<(OwnedSemaphorePermit, ActivityLease)> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.closed.is_cancelled() {
            return None;
        }
        let id = state.next;
        let Some(next) = id.checked_add(1) else {
            self.closed.cancel();
            return None;
        };
        let permit = state
            .idle
            .take()
            .or_else(|| self.slots.clone().try_acquire_owned().ok());
        let Some(permit) = permit else {
            self.closed.cancel();
            return None;
        };
        state.next = next;
        state.active.insert(id, StreamDelivery::default());
        state.idle_deadline = None;
        self.publish_deadline(&state);
        Some((
            permit,
            ActivityLease {
                activity: self.clone(),
                id,
            },
        ))
    }
    pub async fn expired(&self) {
        let mut deadlines = self.idle.subscribe();
        loop {
            let deadline = *deadlines.borrow_and_update();
            if deadline.is_some_and(|at| at <= tokio::time::Instant::now()) {
                let _state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *self.idle.borrow() == deadline {
                    return;
                }
            }
            tokio::select! { biased;
                () = self.closed.cancelled() => return,
                result = deadlines.changed() => { if result.is_err() { return; } },
                () = async {
                    if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; }
                    else { std::future::pending::<()>().await; }
                } => {
                    let _state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if *self.idle.borrow() == deadline { return; }
                }
            }
        }
    }
}
impl ActivityLease {
    fn update(&self, change: impl FnOnce(&mut StreamDelivery)) {
        let mut state = self
            .activity
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        change(
            state
                .active
                .get_mut(&self.id)
                .expect("live delivery owns its entry"),
        );
        self.activity.publish_deadline(&state);
    }
}
impl Drop for ActivityLease {
    fn drop(&mut self) {
        let mut state = self
            .activity
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active.remove(&self.id);
        if state.active.is_empty() {
            state.idle = self.activity.slots.clone().try_acquire_owned().ok();
            if state.idle.is_none() {
                self.activity.closed.cancel();
            }
            state.idle_deadline =
                Some(tokio::time::Instant::now() + std::time::Duration::from_secs(10));
        }
        self.activity.publish_deadline(&state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::{io::AsyncWriteExt, sync::Semaphore};

    #[derive(Default)]
    struct Gate(Arc<AtomicBool>);
    impl AsyncWrite for Gate {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            if self.0.load(Ordering::Acquire) {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn active_delivery_has_no_idle_deadline_and_flush_restarts_idle_ownership() {
        let slots = Arc::new(Semaphore::new(1));
        let activity = Activity::new(slots.clone(), slots.clone().try_acquire_owned().unwrap());
        let flush = Arc::new(Flush::default());
        let delivery = Delivery::for_h2(&activity, flush.clone()).unwrap();
        tokio::time::advance(std::time::Duration::from_secs(11)).await;
        assert!(activity.expired().now_or_never().is_none());
        drop(delivery);
        assert!(
            activity.expired().now_or_never().is_none(),
            "queued output remains active"
        );
        let gate = Gate::default();
        gate.0.store(true, Ordering::Release);
        FlushIo::new(gate, flush).flush().await.unwrap();
        assert_eq!(
            slots.available_permits(),
            0,
            "idle connection must retain admission"
        );
        assert!(activity.expired().now_or_never().is_none());
        tokio::time::advance(std::time::Duration::from_secs(10)).await;
        assert!(activity.expired().now_or_never().is_some());
        drop(activity);
        assert_eq!(slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn last_queued_frame_keeps_its_admission_until_transport_flush() {
        let slots = Arc::new(Semaphore::new(1));
        let flush = Arc::new(Flush::default());
        let delivery = Delivery::new(
            slots.clone().try_acquire_owned().unwrap(),
            Some(flush.clone()),
        );
        let body = http_body_util::Full::new(Bytes::from_static(b"frame"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let mut body = delivery.body(body);
        let bytes = body.frame().await.unwrap().unwrap().into_data().unwrap();
        let slice = bytes.slice(1..);
        drop(delivery);
        drop(body);
        drop(bytes);
        assert_eq!(slots.available_permits(), 0);
        let gate = Gate::default();
        let ready = gate.0.clone();
        let mut io = FlushIo::new(gate, flush);
        // Even a flush cannot release authority while a codec frame is retained.
        ready.store(true, Ordering::Release);
        io.flush().await.unwrap();
        assert_eq!(slots.available_permits(), 0);
        ready.store(false, Ordering::Release);
        drop(slice);
        assert!(io.flush().now_or_never().is_none());
        assert_eq!(slots.available_permits(), 0);
        ready.store(true, Ordering::Release);
        io.flush().await.unwrap();
        assert_eq!(slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn socket_disposal_releases_queued_and_later_dropped_frames() {
        let slots = Arc::new(Semaphore::new(2));
        let flush = Arc::new(Flush::default());
        let first = Delivery::new(
            slots.clone().try_acquire_owned().unwrap(),
            Some(flush.clone()),
        );
        let second = Delivery::new(
            slots.clone().try_acquire_owned().unwrap(),
            Some(flush.clone()),
        );
        drop(first);
        assert_eq!(slots.available_permits(), 0);
        drop(FlushIo::new(Gate::default(), flush));
        assert_eq!(slots.available_permits(), 1);
        drop(second);
        assert_eq!(slots.available_permits(), 2);
    }
    #[tokio::test(start_paused = true)]
    async fn unpublished_bytes_and_idle_subscriptions_have_distinct_deadlines() {
        let slots = Arc::new(Semaphore::new(2));
        let activity = Activity::new(slots.clone(), slots.clone().try_acquire_owned().unwrap());
        let flush = Arc::new(Flush::default());
        activity.track_flush(&flush);
        let delivery = Delivery::for_h2(&activity, flush.clone()).unwrap();
        delivery.begin_body(true);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
        sender.send(Bytes::from_static(b": ready\n\n")).unwrap();
        let body = http_body_util::StreamBody::new(futures_util::stream::unfold(
            receiver,
            |mut receiver| async {
                receiver.recv().await.map(|bytes| {
                    (
                        Ok::<_, std::io::Error>(hyper::body::Frame::data(bytes)),
                        receiver,
                    )
                })
            },
        ))
        .boxed_unsync();
        let mut body = delivery.body(body);
        drop(body.frame().await.unwrap().unwrap());
        let gate = Gate::default();
        gate.0.store(true, Ordering::Release);
        let mut io = FlushIo::new(gate, flush);
        io.flush().await.unwrap();
        tokio::time::advance(std::time::Duration::from_secs(31)).await;
        assert!(
            activity.expired().now_or_never().is_none(),
            "idle subscription must survive"
        );
        sender.send(Bytes::from_static(b"data: live\n\n")).unwrap();
        let frame = body.frame().await.unwrap().unwrap();
        io.flush().await.unwrap();
        tokio::time::advance(std::time::Duration::from_secs(29)).await;
        assert!(activity.expired().now_or_never().is_none());
        // Other successful writes cannot renew a frame still held by the codec.
        io.flush().await.unwrap();
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert!(activity.expired().now_or_never().is_some());
        drop((frame, body, delivery, io, activity));
        assert_eq!(slots.available_permits(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn response_deadline_does_not_require_the_codec_to_poll_its_body() {
        for streaming in [false, true] {
            let slots = Arc::new(Semaphore::new(1));
            let activity = Activity::new(slots.clone(), slots.clone().try_acquire_owned().unwrap());
            let delivery = Delivery::for_h2(&activity, Arc::new(Flush::default())).unwrap();
            delivery.begin_body(streaming);
            tokio::time::advance(std::time::Duration::from_secs(30)).await;
            assert!(activity.expired().now_or_never().is_some());
        }
    }
}
