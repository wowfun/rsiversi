use super::*;
use rsi_agent_session_protocol::SessionHeader;
use rsi_agent_store_protocol::SessionStore;

pub(crate) struct FragmentReader {
    routes: rsi_api_http::ApplicationUpstream,
    callback: rsi_api_http::ApplicationCallback,
    pages: Pages,
    revoked: tokio_util::sync::CancellationToken,
}
impl FragmentReader {
    pub(crate) fn new() -> Self {
        let routes = rsi_api_http::ApplicationUpstream::preparing(
            rsi_credentials_protocol::SecretValue::new("a".repeat(64)).unwrap(),
            vec![],
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let revoked = tokio_util::sync::CancellationToken::new();
        let callback = routes
            .open_callback(&rsi_api_protocol::AuthenticatedDevice {
                id: rsi_api_protocol::DeviceId::generate().unwrap(),
                revoked: revoked.clone(),
            })
            .unwrap();
        Self {
            routes,
            callback,
            pages: Pages::default(),
            revoked,
        }
    }
    async fn read(
        &self,
        key: &str,
        offset: usize,
        identity: Identity,
        load: impl Future<Output = Result<(u64, String)>>,
    ) -> Result<Value> {
        self.pages
            .read(
                &self.routes,
                self.callback.id(),
                key,
                offset,
                identity,
                load,
            )
            .await
    }
    pub(crate) async fn facts(
        &self,
        store: &dyn SessionStore,
        session: &SessionId,
        after: u64,
        through: u64,
        offset: usize,
    ) -> Result<Value> {
        self.pages
            .read(
                &self.routes,
                self.callback.id(),
                "facts",
                offset,
                Identity::Facts {
                    session: session.clone(),
                    parent: None,
                    after,
                    through,
                },
                super::super::session_history::read_facts(store, session, after, through),
            )
            .await
    }
    pub(crate) async fn controls(
        &self,
        store: &dyn SessionStore,
        header: &SessionHeader,
        after: u64,
        through: u64,
        offset: usize,
    ) -> Result<Value> {
        self.pages
            .read(
                &self.routes,
                self.callback.id(),
                "controls",
                offset,
                Identity::Controls {
                    session: header.session_id().clone(),
                    parent: None,
                    after,
                    through,
                },
                super::super::session_controls::read_controls(store, header, after, through),
            )
            .await
    }
    pub(crate) async fn message(
        &self,
        store: &dyn SessionStore,
        session: &SessionId,
        message: &rsi_agent_session_protocol::MessageId,
        accepted: u64,
        offset: usize,
    ) -> Result<Value> {
        self.pages
            .read(
                &self.routes,
                self.callback.id(),
                "message",
                offset,
                Identity::Message {
                    session: session.clone(),
                    parent: None,
                    message: message.clone(),
                    accepted,
                },
                super::super::session_history::read_message(store, session, message, accepted),
            )
            .await
    }
}

#[tokio::test]
async fn continuation_preserves_parent_authority_and_rejects_revoked_callers() {
    let identities = |parent: Option<&str>| {
        let session = SessionId::new("child").unwrap();
        let parent = parent.map(|value| SessionId::new(value).unwrap());
        [
            Identity::Facts {
                session: session.clone(),
                parent: parent.clone(),
                after: 0,
                through: 1,
            },
            Identity::Controls {
                session: session.clone(),
                parent: parent.clone(),
                after: 0,
                through: 1,
            },
            Identity::Message {
                session,
                parent,
                message: MessageId::new("message").unwrap(),
                accepted: 1,
            },
        ]
    };
    for ((original, altered), same) in identities(Some("parent"))
        .into_iter()
        .zip(identities(None))
        .zip(identities(Some("parent")))
    {
        let reader = FragmentReader::new();
        let first = reader
            .read("page", 0, original, async {
                Ok((
                    1,
                    format!("[\"{}\"]", "x".repeat(rsi_extension::MAXIMUM_FRAME_BYTES)),
                ))
            })
            .await
            .unwrap();
        let offset = usize::try_from(first["next_offset"].as_u64().unwrap()).unwrap();
        assert!(matches!(
            reader
                .read("page", offset, altered, async {
                    panic!("continuation must not query metadata or bodies")
                })
                .await,
            Err(Error::Protocol)
        ));
        reader.revoked.cancel();
        assert!(matches!(
            reader
                .read("page", offset, same, async {
                    panic!("revoked continuation must not load")
                })
                .await,
            Err(Error::Cancelled)
        ));
    }
}

#[tokio::test]
async fn retained_pages_bind_the_request_and_coordinates_and_release_capacity() {
    let reader = FragmentReader::new();
    let identity = |session: &str| Identity::Facts {
        session: SessionId::new(session).unwrap(),
        parent: None,
        after: 0,
        through: 1,
    };
    let body = || format!("[\"{}\"]", "界".repeat(350_000));
    let first = reader
        .read("first", 0, identity("one"), async { Ok((1, body())) })
        .await
        .unwrap();
    let offset = usize::try_from(first["next_offset"].as_u64().unwrap()).unwrap();
    assert!(
        reader
            .read("first", offset, identity("other"), async {
                panic!("continuation must not load")
            })
            .await
            .is_err()
    );
    let other = reader
        .routes
        .open_callback(&rsi_api_protocol::AuthenticatedDevice {
            id: rsi_api_protocol::DeviceId::generate().unwrap(),
            revoked: tokio_util::sync::CancellationToken::new(),
        })
        .unwrap();
    assert!(
        reader
            .pages
            .read(
                &reader.routes,
                other.id(),
                "first",
                offset,
                identity("one"),
                async { panic!("foreign continuation must not load") }
            )
            .await
            .is_err()
    );
    reader
        .read("second", 0, identity("two"), async { Ok((1, body())) })
        .await
        .unwrap();
    assert!(matches!(
        reader
            .read("third", 0, identity("three"), async {
                panic!("capacity must precede loading")
            })
            .await,
        Err(Error::Capacity)
    ));
    let tail = reader
        .read("first", offset, identity("one"), async {
            panic!("continuation must not load")
        })
        .await
        .unwrap();
    assert_eq!(tail["next_offset"], tail["total_bytes"]);
    assert_eq!(
        format!(
            "{}{}",
            first["json"].as_str().unwrap(),
            tail["json"].as_str().unwrap()
        ),
        body()
    );
    reader
        .read("third", 0, identity("three"), async { Ok((1, body())) })
        .await
        .unwrap();
    reader
        .routes
        .release_resource(reader.callback.id(), "history:second");
    assert!(
        reader
            .read("second", offset, identity("two"), async {
                panic!("closed continuation must not load")
            })
            .await
            .is_err()
    );
    // Losing the physical owner releases the remaining page as well.
    drop(reader.callback);
    for key in ["replacement-one", "replacement-two"] {
        reader
            .pages
            .read(&reader.routes, other.id(), key, 0, identity("one"), async {
                Ok((1, body()))
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn failed_cancelled_and_single_fragment_reads_leave_no_retained_page() {
    let reader = FragmentReader::new();
    let identity = || Identity::Facts {
        session: SessionId::new("one").unwrap(),
        parent: None,
        after: 0,
        through: 1,
    };
    let mut pending = Box::pin(reader.read("pending", 0, identity(), std::future::pending()));
    tokio::select! { biased;
        _ = &mut pending => panic!("pending load returned"),
        () = tokio::task::yield_now() => {}
    }
    drop(pending);
    assert!(
        reader
            .read("failed", 0, identity(), async { Err(Error::Protocol) })
            .await
            .is_err()
    );
    for _ in 0..128 {
        reader
            .read("small", 0, identity(), async { Ok((1, "[]".into())) })
            .await
            .unwrap();
    }
    for key in ["first", "second"] {
        reader
            .read(key, 0, identity(), async {
                Ok((
                    1,
                    format!("[\"{}\"]", "x".repeat(rsi_extension::MAXIMUM_FRAME_BYTES)),
                ))
            })
            .await
            .unwrap();
    }
}
