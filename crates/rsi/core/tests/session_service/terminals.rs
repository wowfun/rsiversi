//! Linux native PTY through the authenticated UDS Session API.
use super::*;
use rsi_session_protocol::{
    SessionHandle,
    terminal::{Attachment, InputState, Operation, PtyError, Reply, Request, Size},
};
async fn create(daemon: &DaemonFixture, fixture: &Fixture, id: &str) -> Arc<dyn SessionHandle> {
    daemon
        .connection
        .session_service()
        .create(CreateSession {
            workspace_id: daemon
                .connection
                .workspace_registry()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: SessionId::new(id).unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap()
}
async fn output(handle: &Arc<dyn SessionHandle>, attachment: &Attachment, needle: &str) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut text = String::new();
        let mut cursor = 0;
        let mut epoch = attachment.stream_epoch;
        loop {
            let Reply::Output(page) = handle
                .terminal(Request::Operate {
                    operation: Operation::Read {
                        terminal: attachment.terminal.id.clone(),
                        attachment: attachment.id.clone(),
                        stream_epoch: epoch,
                        cursor,
                    },
                })
                .await
                .unwrap()
            else {
                panic!()
            };
            epoch = page.stream_epoch;
            cursor = page.next_cursor;
            text.push_str(&page.text);
            if text.contains(needle) {
                return text;
            }
            assert!(text.len() < 256 * 1024);
        }
    })
    .await
    .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires native Linux Bubblewrap namespaces and a controlling PTY"]
#[allow(clippy::too_many_lines)] // One lifecycle proves the exact public API and retained IDs across restart.
async fn uds_terminals_require_persistence_isolate_sessions_keep_facts_unchanged_and_retire() {
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let daemon = DaemonFixture::new(&fixture).await;
    let handle = create(&daemon, &fixture, "terminal-session").await;
    let request = Request::Create {
        size: Size {
            rows: 24,
            columns: 80,
        },
    };
    assert!(matches!(
        handle.terminal(request.clone()).await,
        Err(SessionError::Terminal(PtyError::Unavailable(_)))
    ));
    run_message_to_terminal(&handle, "publish-terminal").await;
    let before = handle.history_before(None, 128).await.unwrap().durable_seq;
    let Reply::Attached(writer) = handle.terminal(request.clone()).await.unwrap() else {
        panic!()
    };
    let other = create(&daemon, &fixture, "other-terminal-session").await;
    run_message_to_terminal(&other, "publish-other").await;
    assert!(matches!(
        other
            .terminal(Request::Operate {
                operation: Operation::Attach {
                    terminal: writer.terminal.id.clone()
                }
            })
            .await,
        Err(SessionError::Terminal(PtyError::Unavailable(_)))
    ));
    let bytes =
        b"printf 'saved by PTY' > terminal-result.txt; printf '\\036RSI_NATIVE_READY\\037\\n'\n"
            .to_vec();
    let Reply::Input(receipt) = handle
        .terminal(Request::Operate {
            operation: Operation::Input {
                terminal: writer.terminal.id.clone(),
                attachment: writer.id.clone(),
                epoch: 1,
                sequence: 1,
                bytes: bytes.clone(),
            },
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(receipt.result, InputState::Accepted { bytes: bytes.len() });
    output(&handle, &writer, "\x1eRSI_NATIVE_READY\x1f").await;
    assert_eq!(
        std::fs::read(fixture.workspace.join("terminal-result.txt")).unwrap(),
        b"saved by PTY"
    );
    assert_eq!(
        handle.history_before(None, 128).await.unwrap().durable_seq,
        before
    );
    let Reply::Attached(reader) = handle
        .terminal(Request::Operate {
            operation: Operation::Attach {
                terminal: writer.terminal.id.clone(),
            },
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_ne!(
        reader.terminal.controller.as_deref(),
        Some(reader.id.as_str())
    );
    let Reply::Terminal(taken) = handle
        .terminal(Request::Operate {
            operation: Operation::Takeover {
                terminal: writer.terminal.id.clone(),
                attachment: reader.id.clone(),
            },
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(taken.controller_epoch, 2);
    assert!(matches!(
        handle
            .terminal(Request::Operate {
                operation: Operation::Input {
                    terminal: writer.terminal.id.clone(),
                    attachment: writer.id.clone(),
                    epoch: 1,
                    sequence: 2,
                    bytes: b"bad".to_vec()
                }
            })
            .await,
        Err(SessionError::Terminal(PtyError::StaleController))
    ));
    handle
        .terminal(Request::Operate {
            operation: Operation::Detach {
                terminal: writer.terminal.id.clone(),
                attachment: writer.id.clone(),
                epoch: 1,
            },
        })
        .await
        .unwrap();
    let id = handle.header().await.unwrap().session_id().clone();
    drop(handle);
    let resumed = daemon
        .connection
        .session_service()
        .attach(&id)
        .await
        .unwrap();
    let Reply::List(list) = resumed
        .terminal(Request::Operate {
            operation: Operation::List,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(list.len(), 1);
    resumed
        .terminal(Request::Operate {
            operation: Operation::CloseAll,
        })
        .await
        .unwrap();
    assert_eq!(
        resumed
            .terminal(Request::Operate {
                operation: Operation::List
            })
            .await
            .unwrap(),
        Reply::List(vec![])
    );
    let Reply::Attached(retired) = resumed.terminal(request).await.unwrap() else {
        panic!()
    };
    daemon.shutdown().await;
    drop(resumed);
    drop(other);
    let restarted = DaemonFixture::new(&fixture).await;
    let attached = restarted
        .connection
        .session_service()
        .attach(&id)
        .await
        .unwrap();
    assert_eq!(
        attached
            .terminal(Request::Operate {
                operation: Operation::List
            })
            .await
            .unwrap(),
        Reply::List(vec![])
    );
    assert!(matches!(
        attached
            .terminal(Request::Operate {
                operation: Operation::Attach {
                    terminal: retired.terminal.id
                }
            })
            .await,
        Err(SessionError::Terminal(PtyError::Unavailable(_)))
    ));
    assert_eq!(
        attached
            .history_before(None, 128)
            .await
            .unwrap()
            .durable_seq,
        before
    );
    restarted.shutdown().await;
    provider.abort();
}
