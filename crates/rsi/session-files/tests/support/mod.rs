use async_trait::async_trait;
use rsi_agent_session_protocol::SessionId;
use rsi_api_protocol::{ApiClient, ApiError};
use rsi_api_protocol::{
    ApiMessage, ApiOutput, ByteBudget, ConnectionDescription, EndpointId, HostEpoch,
    OperationClass, OperationSpec, RetainedBytes,
};
use rsi_files_protocol::{FileKind, OpenedFile, RelativePath};
use rsi_session_files::{
    FilesOperation, Result, SessionFiles as _, SessionFilesClient, SessionFilesError,
};
use rsi_session_protocol::SessionTarget;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
struct Remote {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    replacement: Mutex<Option<Value>>,
    request_override: Mutex<Option<Value>>,
    calls: AtomicUsize,
}
impl Remote {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            description: ConnectionDescription {
                wire_version: 1,
                endpoint_id: EndpointId::from_bytes([1; 16]),
                host_epoch: HostEpoch::from_bytes([2; 16]),
            },
            operations: [
                FilesOperation::Open,
                FilesOperation::Read,
                FilesOperation::List,
                FilesOperation::Release,
            ]
            .map(FilesOperation::spec)
            .into(),
            replacement: Mutex::new(None),
            request_override: Mutex::new(None),
            calls: AtomicUsize::new(0),
        })
    }
    fn body(&self, value: Value) {
        *self.replacement.lock().unwrap() = Some(value);
    }
}
#[async_trait]
impl ApiClient for Remote {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(
        &self,
        _: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let request = self
            .request_override
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| serde_json::from_slice(input.as_bytes()).unwrap());
        let value =
            json!({"request": request, "body": self.replacement.lock().unwrap().take().unwrap()});
        Ok(ApiOutput::Reply(ApiMessage {
            json: ByteBudget::default()
                .encode(&value, FilesOperation::Read.spec().maximum_response_bytes)
                .unwrap(),
            binary: None,
        }))
    }
}
fn target() -> SessionTarget {
    SessionTarget {
        session_id: SessionId::new("client-files").unwrap(),
        header_key: "a".repeat(64),
    }
}
fn file(kind: FileKind) -> OpenedFile {
    OpenedFile {
        path: RelativePath::new(b"sub").unwrap(),
        token: "0".repeat(32).try_into().unwrap(),
        kind,
        length: 2,
    }
}
fn is_malformed<T>(result: &Result<T>) -> bool {
    matches!(result, Err(SessionFilesError::Api(ApiError::Invalid(_))))
}

pub async fn malformed_open_binding_and_file_pages_never_escape_or_replay() {
    let remote = Remote::new();
    let client = SessionFilesClient::new(remote.clone()).unwrap();
    for body in [
        json!({"path": "78", "kind": "file", "length": 2, "token": "0".repeat(32)}),
        json!({"path": "737562", "kind": "directory", "length": 2, "token": "0".repeat(32)}),
        json!({"path": "737562", "kind": "file", "length": 2, "token": "unknown"}),
    ] {
        remote.body(body);
        assert!(is_malformed(
            &client
                .open(target(), RelativePath::new(b"sub").unwrap(), FileKind::File)
                .await
        ));
    }
    for body in [
        json!({"offset": 1, "total": 2, "bytes_hex": "0001"}),
        json!({"offset": 0, "total": 3, "bytes_hex": "0001"}),
        json!({"offset": 0, "total": 2, "bytes_hex": "00"}),
        json!({"offset": 0, "total": 2, "bytes_hex": "0g01"}),
        json!({"offset": 0, "total": 2, "bytes_hex": "000102"}),
    ] {
        remote.body(body);
        assert!(is_malformed(
            &client.read(target(), file(FileKind::File), 0, 2).await
        ));
    }
    remote.body(json!({"offset": 0, "total": 2, "bytes_hex": "0001"}));
    *remote.request_override.lock().unwrap() = Some(
        json!({"target": {"session_id": "other", "header_key": "a".repeat(64)}, "input": {"file": file(FileKind::File), "offset": 0, "maximum": 2}}),
    );
    assert!(is_malformed(
        &client.read(target(), file(FileKind::File), 0, 2).await
    ));
    assert_eq!(remote.calls.load(Ordering::SeqCst), 9);
    assert!(
        client
            .read(target(), file(FileKind::File), u64::MAX, 2)
            .await
            .is_err()
    );
    assert_eq!(remote.calls.load(Ordering::SeqCst), 9);
}

pub async fn directory_client_checks_parent_exact_names_order_and_forward_progress() {
    let remote = Remote::new();
    let client = SessionFilesClient::new(remote.clone()).unwrap();
    let a = json!({"path": "7375622f61", "name": "a", "kind": "file"});
    let b = json!({"path": "7375622f62", "name": "b", "kind": null});
    let other = json!({"path": "6f746865722f61", "name": "a", "kind": "file"});
    let renamed = json!({"path": "7375622f61", "name": "wrong", "kind": "file"});
    for entries in [
        json!([]),
        json!([a.clone(), a.clone()]),
        json!([b.clone(), a.clone()]),
        json!([other]),
        json!([renamed]),
        json!([a.clone(), b.clone(), a.clone()]),
    ] {
        remote.body(json!({"offset": 0, "total": 2, "entries": entries}));
        assert!(is_malformed(
            &client.list(target(), file(FileKind::Directory), 0, 2).await
        ));
    }
    remote.body(json!({"offset": 0, "total": 2, "entries": [a, b]}));
    assert_eq!(
        client
            .list(target(), file(FileKind::Directory), 0, 2)
            .await
            .unwrap()
            .entries
            .len(),
        2
    );
    assert_eq!(remote.calls.load(Ordering::SeqCst), 7);
}
