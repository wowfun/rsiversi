use std::{future::Future, time::Duration};

pub async fn read_until_ready<F, Fut>(wait: bool, mut read: F) -> rsi_lsp::Result<rsi_lsp::Output>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = rsi_lsp::Result<rsi_lsp::Output>>,
{
    if !wait {
        return read().await;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let result = tokio::time::timeout_at(deadline, read())
            .await
            .map_err(|_| rsi_lsp::Error::Deadline)?;
        let empty = match &result {
            Ok(output) => match &output.result {
                rsi_lsp::QueryResult::Locations { locations } => locations.is_empty(),
                rsi_lsp::QueryResult::Hover { text, .. } => text.is_empty(),
            },
            Err(rsi_lsp::Error::Server(-32801)) => true,
            Err(_) => false,
        };
        if !empty || tokio::time::Instant::now() >= deadline {
            return result;
        }
        tokio::time::sleep_until(
            (tokio::time::Instant::now() + Duration::from_millis(50)).min(deadline),
        )
        .await;
        if tokio::time::Instant::now() >= deadline {
            return result;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_lsp::{Error, Operation, Output, Query, QueryResult};
    fn output(text: &str) -> Output {
        Output {
            query: Query {
                operation: Operation::Hover,
                path: "src/main.rs".into(),
                line: 1,
                column: 1,
            },
            result: QueryResult::Hover {
                text: text.into(),
                range: None,
            },
        }
    }
    #[tokio::test(start_paused = true)]
    async fn retries_only_content_modified_while_indexing() {
        let mut replies = [
            Err(Error::Server(-32801)),
            Ok(output("")),
            Ok(output("Bird")),
        ]
        .into_iter();
        let mut calls = 0;
        let result = read_until_ready(true, || {
            calls += 1;
            std::future::ready(replies.next().unwrap())
        })
        .await;
        assert_eq!(result, Ok(output("Bird")));
        assert_eq!(calls, 3);
    }
    #[tokio::test(start_paused = true)]
    async fn ordinary_reads_and_other_errors_never_repeat() {
        for (wait, reply) in [
            (false, Err(Error::Server(-32801))),
            (false, Ok(output(""))),
            (true, Err(Error::Server(-32603))),
            (true, Err(Error::OutcomeUnknown)),
            (true, Err(Error::Cancelled)),
        ] {
            let mut calls = 0;
            let result = read_until_ready(wait, || {
                calls += 1;
                std::future::ready(reply.clone())
            })
            .await;
            assert_eq!(result, reply);
            assert_eq!(calls, 1);
        }
    }
    #[tokio::test(start_paused = true)]
    async fn in_flight_reads_cannot_extend_the_indexing_window() {
        let started = tokio::time::Instant::now();
        let mut calls = 0;
        let result = read_until_ready(true, || {
            calls += 1;
            async {
                tokio::time::sleep(Duration::from_secs(6)).await;
                Ok(output(""))
            }
        })
        .await;
        assert_eq!(result, Err(Error::Deadline));
        assert_eq!(
            tokio::time::Instant::now() - started,
            Duration::from_secs(10)
        );
        assert_eq!(calls, 2);
    }
}
