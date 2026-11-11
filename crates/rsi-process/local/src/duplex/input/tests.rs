use super::*;
use std::{
    future::Future as _,
    pin::pin,
    task::{Context, Poll, Waker},
    time::Duration,
};
use tokio::io::AsyncReadExt as _;

#[tokio::test(start_paused = true)]
async fn close_waits_for_buffered_bytes_before_acknowledging_eof() {
    let (writer, mut reader) = tokio::io::duplex(1);
    let (input, receiver) = Input::new(Arc::new(()));
    let task = tokio::spawn(write_input(
        tokio::io::BufWriter::new(writer),
        receiver,
        CancellationToken::new(),
    ));
    assert_eq!(input.write(b"ab").await.unwrap(), 2);
    let mut close = pin!(input.close());
    assert!(matches!(
        close.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(matches!(
        close.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    assert!(matches!(
        input.write(b"c").await,
        Err(ProcessError::Capacity)
    ));
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await.unwrap();
    close.await.unwrap();
    task.await.unwrap();
    assert_eq!(bytes, b"ab");
}

#[tokio::test(start_paused = true)]
async fn abandoned_waiter_keeps_its_slot_and_delivers_once_after_backpressure() {
    let (writer, mut reader) = tokio::io::duplex(1);
    let (input, receiver) = Input::new(Arc::new(()));
    let stop = CancellationToken::new();
    let task = tokio::spawn(write_input(writer, receiver, stop));
    assert_eq!(input.write(b"a").await.unwrap(), 1);
    {
        let mut blocked = pin!(input.write(b"b"));
        assert!(matches!(
            blocked
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        // With paused time this advances only after runnable pipe work settles.
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(matches!(
            blocked
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
    }
    assert!(matches!(
        input.write(b"c").await,
        Err(ProcessError::Capacity)
    ));
    let mut bytes = [0; 2];
    reader.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"ab");
    drop(input.slot.acquire().await.unwrap());
    input.close().await.unwrap();
    assert_eq!(reader.read(&mut bytes).await.unwrap(), 0);
    task.await.unwrap();
    assert!(matches!(
        input.write(b"c").await,
        Err(ProcessError::ShuttingDown)
    ));
}

#[tokio::test(start_paused = true)]
async fn process_cancellation_ends_a_blocked_write_and_releases_the_pipe() {
    let (writer, mut reader) = tokio::io::duplex(1);
    let (input, receiver) = Input::new(Arc::new(()));
    let stop = CancellationToken::new();
    let task = tokio::spawn(write_input(writer, receiver, stop.clone()));
    assert_eq!(input.write(b"a").await.unwrap(), 1);
    let mut blocked = pin!(input.write(b"b"));
    assert!(matches!(
        blocked
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    tokio::time::sleep(Duration::from_secs(2)).await;
    stop.cancel();
    assert!(matches!(blocked.await, Err(ProcessError::Io(_))));
    task.await.unwrap();
    let mut remaining = Vec::new();
    reader.read_to_end(&mut remaining).await.unwrap();
    assert_eq!(remaining, b"a");
    assert_eq!(input.slot.available_permits(), 1);
    assert!(matches!(
        input.write(b"c").await,
        Err(ProcessError::ShuttingDown)
    ));
}
