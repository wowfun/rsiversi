use super::{Arc, Connection, Entry, JoinSet, Native, SendStream};
use rsi_process::ProcessOutput;
use rsi_ssh_protocol::frame::MAXIMUM_FRAGMENT_BYTES;
use std::time::Duration;

pub(super) async fn run(
    entry: Arc<Entry>,
    native: Native,
    output: SendStream,
    error: Option<SendStream>,
    connection: Connection,
) {
    let mut pumps = JoinSet::new();
    match &native {
        Native::Batch(process) => {
            pumps.spawn(tail(process.stdout(), output, entry.clone()));
            if let Some(error) = error {
                pumps.spawn(tail(process.stderr(), error, entry.clone()));
            }
        }
        Native::Duplex(process) => {
            pumps.spawn(lossless(native.clone(), output, entry.clone()));
            if let Some(error) = error {
                pumps.spawn(tail(process.stderr(), error, entry.clone()));
            }
        }
        Native::Pty(_) => {
            pumps.spawn(lossless(native.clone(), output, entry.clone()));
        }
    }
    let outcome = tokio::select! {
        outcome = native.wait() => outcome,
        () = entry.cancel.cancelled() => { native.terminate(); native.wait().await },
        () = connection.closed() => { entry.cancel.cancel(); native.terminate(); native.wait().await },
    };
    let settlement = native.settlement(&outcome).await;
    entry.settlement.send_replace(Some(settlement));
    entry.outcome.send_replace(Some(outcome));
    // A settled child may still have credited output waiting for a live reader.
    // Release/connection close cancels pumps; native settlement has already finished.
    while let Some(result) = pumps.join_next().await {
        if result.is_err() {
            connection.close();
            entry.cancel.cancel();
        }
    }
}
async fn send(output: &mut SendStream, bytes: Vec<u8>, eof: bool, entry: &Entry) -> bool {
    tokio::select! {
        () = entry.cancel.cancelled() => false,
        result = output.send(bytes, eof) => result.is_ok(),
    }
}
async fn tail(source: Arc<dyn ProcessOutput>, mut output: SendStream, entry: Arc<Entry>) {
    let mut offset = 0u64;
    loop {
        let settled = entry.outcome.borrow().is_some();
        let Ok(read) = source.read_from(offset) else {
            let _ = send(&mut output, vec![1], true, &entry).await;
            return;
        };
        let beginning = read.next_offset.saturating_sub(read.bytes.len() as u64);
        if !read.bytes.is_empty() || settled {
            for start in (0..read.bytes.len().max(1)).step_by(MAXIMUM_FRAGMENT_BYTES - 8) {
                let end = (start + MAXIMUM_FRAGMENT_BYTES - 8).min(read.bytes.len());
                let eof = settled && end == read.bytes.len();
                let Ok(packet) = rsi_ssh_protocol::rpc::encode_tail(
                    beginning + start as u64,
                    &read.bytes[start..end],
                ) else {
                    let _ = send(&mut output, vec![1], true, &entry).await;
                    return;
                };
                if !send(&mut output, packet, eof, &entry).await {
                    return;
                }
            }
            offset = read.next_offset;
            if settled {
                return;
            }
        } else {
            tokio::select! { () = entry.cancel.cancelled() => return, () = tokio::time::sleep(Duration::from_millis(10)) => {} }
        }
    }
}
async fn lossless(native: Native, mut output: SendStream, entry: Arc<Entry>) {
    loop {
        let result = tokio::select! {
            () = entry.cancel.cancelled() => return,
            result = async {
                match &native {
                    Native::Duplex(process) => process.stdout().read(MAXIMUM_FRAGMENT_BYTES - 1).await.map(|read| (read.bytes, read.eof)),
                    Native::Pty(process) => process.read().await.map(|read| (read.bytes, read.eof)),
                    Native::Batch(_) => unreachable!(),
                }
            } => result,
        };
        let Ok((bytes, eof)) = result else {
            let _ = send(&mut output, vec![1], true, &entry).await;
            return;
        };
        if bytes.is_empty() {
            if eof {
                let _ = send(&mut output, vec![0], true, &entry).await;
            }
            return;
        }
        let chunks = bytes.chunks(MAXIMUM_FRAGMENT_BYTES - 1);
        let count = chunks.len();
        for (index, chunk) in chunks.enumerate() {
            let mut packet = Vec::with_capacity(chunk.len() + 1);
            packet.push(0);
            packet.extend_from_slice(chunk);
            if !send(&mut output, packet, eof && index + 1 == count, &entry).await {
                return;
            }
        }
        if eof {
            return;
        }
    }
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
