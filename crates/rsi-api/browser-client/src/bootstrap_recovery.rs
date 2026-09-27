//! A cookie read can recover an exchange, but cannot replace its failure evidence.
use rsi_api_protocol::Result;
use std::future::Future;

pub(crate) async fn recover<T, F: Future<Output = Result<T>>>(
    ticket_present: bool,
    exchanged: Result<T>,
    cookie: impl FnOnce() -> F,
) -> Result<T> {
    match exchanged {
        Err(first @ rsi_api_protocol::ApiError::OutcomeUnknown) if ticket_present => {
            cookie().await.map_err(|_| first)
        }
        result => result,
    }
}

pub(crate) fn after_cleanup<T>(result: Result<T>, cleanup: Result<()>) -> Result<T> {
    match result {
        Err(error) => Err(error),
        Ok(value) => cleanup.map(|()| value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_api_protocol::ApiError;
    use std::{
        pin::pin,
        task::{Context, Poll, Waker},
    };

    fn ready<T>(future: impl Future<Output = T>) -> T {
        let Poll::Ready(value) = pin!(future)
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        else {
            panic!("fixture contains only immediately ready I/O");
        };
        value
    }
    #[test]
    fn cleanup_preserves_original_failure_and_never_publishes_undrained_success() {
        assert!(matches!(
            after_cleanup::<()>(Err(ApiError::OutcomeUnknown), Err(ApiError::Capacity)),
            Err(ApiError::OutcomeUnknown)
        ));
        assert!(matches!(
            after_cleanup(Ok(42), Err(ApiError::Capacity)),
            Err(ApiError::Capacity)
        ));
    }
    #[test]
    fn failed_cookie_recovery_preserves_the_ticket_exchange_error() {
        let result = ready(recover::<u8, _>(
            true,
            Err(ApiError::OutcomeUnknown),
            || async { Err(ApiError::Unauthorized) },
        ));
        assert!(matches!(result, Err(ApiError::OutcomeUnknown)));
    }
    #[test]
    fn cookie_can_recover_an_uncertain_exchange_without_replaying_it() {
        assert_eq!(
            ready(recover(true, Err(ApiError::OutcomeUnknown), || async {
                Ok(42)
            }))
            .unwrap(),
            42
        );
    }
    #[test]
    fn definitive_rejection_cannot_fall_back_to_a_valid_cookie() {
        for error in [
            ApiError::Unauthorized,
            ApiError::Capacity,
            ApiError::ShuttingDown,
            ApiError::Unavailable,
        ] {
            let expected = std::mem::discriminant(&error);
            let result = ready(recover(true, Err(error), || async { Ok(42) }));
            assert_eq!(std::mem::discriminant(&result.unwrap_err()), expected);
        }
    }
    #[test]
    fn success_and_cookie_only_failure_never_start_recovery() {
        assert_eq!(
            ready(recover(true, Ok(42), || async {
                panic!("unnecessary recovery")
            }))
            .unwrap(),
            42
        );
        let result = ready(recover::<u8, _>(
            false,
            Err(ApiError::Unauthorized),
            || async {
                panic!("cookie-only bootstrap must issue one request");
            },
        ));
        assert!(matches!(result, Err(ApiError::Unauthorized)));
    }
}
