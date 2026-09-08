use crate::{ApiError, ApiMessage, ApiStream, Result};
use futures_util::{Stream, StreamExt, future::BoxFuture};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Creates a one-item stream handoff and its separately owned forwarding driver.
///
/// The owner must drive the returned future until settlement. Dropping the consumer
/// releases its producer; retirement and revocation release an unpolled producer.
/// Reserve precedes polling the source, and failure cannot become a clean EOF.
pub fn supervised_stream(
    mut source: ApiStream,
    maximum_response_bytes: usize,
    retiring: CancellationToken,
    revoked: CancellationToken,
) -> (ApiStream, BoxFuture<'static, ()>) {
    let (sender, receiver) = mpsc::channel(1);
    let (terminal, end) = oneshot::channel();
    let output = Forwarded {
        receiver: Some(receiver),
        terminal: Some(end),
    };
    let driver = Box::pin(async move {
        let end = loop {
            // Reserve the single channel slot before asking the domain to materialize
            // another item. No unbounded producer queue exists behind a slow consumer.
            let permit = tokio::select! {
                biased;
                () = retiring.cancelled() => break Err(ApiError::ShuttingDown),
                () = revoked.cancelled() => break Err(ApiError::Unauthorized),
                permit = sender.reserve() => match permit { Ok(permit) => permit, Err(_) => return },
            };
            let item = tokio::select! {
                biased;
                () = retiring.cancelled() => break Err(ApiError::ShuttingDown),
                () = revoked.cancelled() => break Err(ApiError::Unauthorized),
                () = sender.closed() => return,
                item = source.next() => item,
            };
            match item {
                None => break Ok(()),
                Some(Err(error)) => break Err(error),
                Some(Ok(message)) if message.encoded_len() > maximum_response_bytes => {
                    break Err(ApiError::Backend(
                        "stream item exceeds registered bound".into(),
                    ));
                }
                Some(Ok(message)) => {
                    permit.send(message);
                }
            }
        };
        drop(source);
        drop(sender);
        let _ = terminal.send(end);
    });
    (Box::pin(output), driver)
}

struct Forwarded {
    receiver: Option<mpsc::Receiver<ApiMessage>>,
    terminal: Option<oneshot::Receiver<Result<()>>>,
}
impl Stream for Forwarded {
    type Item = Result<ApiMessage>;
    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.receiver.is_none() {
            return Poll::Ready(None);
        }
        if let Some(terminal) = &mut self.terminal
            && let Poll::Ready(result) = Pin::new(terminal).poll(context)
        {
            self.terminal = None;
            let end = result.unwrap_or_else(|_| {
                Err(ApiError::Backend(
                    "subscription task stopped without an end".into(),
                ))
            });
            if let Err(error) = end {
                self.receiver = None;
                return Poll::Ready(Some(Err(error)));
            }
        }
        match self
            .receiver
            .as_mut()
            .expect("checked above")
            .poll_recv(context)
        {
            Poll::Ready(None) if self.terminal.is_some() => Poll::Pending,
            Poll::Ready(None) => {
                self.receiver = None;
                Poll::Ready(None)
            }
            item => item.map(|item| item.map(Ok)),
        }
    }
}
