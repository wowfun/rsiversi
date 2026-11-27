use http_body_util::Limited;
use hyper::body::Incoming;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioTimer;
use rsi_api_protocol::AuthenticatedDevice;
use rsi_meta::Execution;
use std::{future::Future, sync::Mutex};
use tokio_util::sync::CancellationToken;

type HttpClient = Client<HttpConnector, Limited<Incoming>>;

/// The most recent device can reuse its connection within this process generation.
#[derive(Debug, Default)]
pub(super) struct DeviceClient(Mutex<Option<(AuthenticatedDevice, HttpClient)>>);

impl DeviceClient {
    pub(super) fn for_device(
        &self,
        execution: &Execution,
        retired: &CancellationToken,
        device: &AuthenticatedDevice,
    ) -> HttpClient {
        let mut last = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((previous, client)) = last.as_ref()
            && previous.id == device.id
            && previous.revoked == device.revoked
        {
            return client.clone();
        }
        let mut connector = HttpConnector::new();
        connector.set_connect_timeout(Some(super::TIMEOUT));
        let client = Client::builder(ConnectionTasks {
            execution: execution.clone(),
            retired: retired.clone(),
            revoked: device.revoked.clone(),
        })
        .pool_timer(TokioTimer::new())
        .pool_max_idle_per_host(1)
        .set_host(false)
        .build(connector);
        *last = Some((device.clone(), client.clone()));
        client
    }
}

#[derive(Clone)]
struct ConnectionTasks {
    execution: Execution,
    retired: CancellationToken,
    revoked: CancellationToken,
}

impl<F: Future<Output = ()> + Send + 'static> hyper::rt::Executor<F> for ConnectionTasks {
    fn execute(&self, future: F) {
        let retired = self.retired.clone();
        let revoked = self.revoked.clone();
        drop(self.execution.spawn(async move {
            tokio::select! {
                biased;
                () = retired.cancelled() => {},
                () = revoked.cancelled() => {},
                () = future => {},
            }
        }));
    }
}
