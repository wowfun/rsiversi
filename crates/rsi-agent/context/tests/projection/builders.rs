use super::*;
use rsi_agent_context::{
    ContextBuilderIdentity, ContextInit, ContextPage, DefaultContextBuilder, ModelContextBuilder,
    ModelContextCursor, ModelContextState,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
struct SelectedBuilder {
    identity: ContextBuilderIdentity,
    opened: Arc<AtomicUsize>,
    wrong_restore_position: bool,
}

impl ModelContextBuilder for SelectedBuilder {
    fn identity(&self) -> &ContextBuilderIdentity {
        &self.identity
    }

    fn open(
        &self,
        init: ContextInit<'_>,
    ) -> rsi_agent_context::Result<Box<dyn ModelContextCursor>> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        let restoring = init.checkpoint.is_some();
        let cursor = DefaultContextBuilder::default().open(init)?;
        if restoring && self.wrong_restore_position {
            Ok(Box::new(WrongPosition(cursor)))
        } else {
            Ok(cursor)
        }
    }
}

fn selected(id: &str, version: &str, digest: &str) -> Arc<SelectedBuilder> {
    Arc::new(SelectedBuilder {
        identity: ContextBuilderIdentity::new(id, version, digest).unwrap(),
        opened: Arc::new(AtomicUsize::new(0)),
        wrong_restore_position: false,
    })
}

#[derive(Debug)]
struct WrongPosition(Box<dyn ModelContextCursor>);
impl ModelContextCursor for WrongPosition {
    fn ingest(&mut self, page: ContextPage<'_>) -> rsi_agent_context::Result<()> {
        self.0.ingest(page)
    }
    fn build(
        &self,
        tools: Vec<rsi_tools_protocol::ToolDefinition>,
    ) -> rsi_agent_context::Result<rsi_ai_protocol::LanguageRequest> {
        self.0.build(tools)
    }
    fn checkpoint(&self) -> rsi_agent_context::Result<Arc<[u8]>> {
        self.0.checkpoint()
    }
    fn position(&self) -> rsi_agent_context::ContextPosition {
        let mut position = self.0.position();
        position.through_seq += 1;
        position
    }
}

#[test]
fn restored_provider_position_must_match_the_envelope_before_replacing_state() {
    let provider = Arc::new(SelectedBuilder {
        identity: ContextBuilderIdentity::new("wrong.context", "1.0.0", "a".repeat(64)).unwrap(),
        opened: Arc::new(AtomicUsize::new(0)),
        wrong_restore_position: true,
    });
    let mut state =
        ModelContextState::open(provider, header("system"), ContextLimits::default()).unwrap();
    state
        .ingest(ContextPage::Canonical(&complete_facts()))
        .unwrap();
    let position = state.position();
    let checkpoint = state.checkpoint().unwrap();
    assert!(state.restore(&checkpoint).is_err());
    assert_eq!(state.position(), position);
}

#[test]
fn builder_identity_constructor_and_decode_share_the_same_bounds() {
    let valid = ContextBuilderIdentity::new("test.context", "1.0.0", "a".repeat(64)).unwrap();
    let encoded = serde_json::to_value(&valid).unwrap();
    assert_eq!(
        serde_json::from_value::<ContextBuilderIdentity>(encoded.clone()).unwrap(),
        valid
    );
    for (field, value) in [
        ("id", String::new()),
        ("id", "x".repeat(257)),
        ("id", "bad id".to_owned()),
        ("semantic_version", String::new()),
        ("semantic_version", "v".repeat(65)),
        ("config_sha256", "A".repeat(64)),
        ("config_sha256", "a".repeat(63)),
    ] {
        let mut invalid = encoded.clone();
        invalid[field] = value.into();
        assert!(serde_json::from_value::<ContextBuilderIdentity>(invalid).is_err());
    }
    let mut unknown = encoded;
    unknown["ignored"] = true.into();
    assert!(serde_json::from_value::<ContextBuilderIdentity>(unknown).is_err());
}

fn complete_facts() -> Vec<Arc<SessionFact>> {
    let turn = TurnId::new("turn-builder").unwrap();
    facts(vec![
        SessionFactBody::TurnAccepted {
            turn_id: turn.clone(),
            text: "retained input".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::TurnTerminal {
            turn_id: turn,
            outcome: TurnOutcome::Completed,
        },
    ])
    .into_iter()
    .map(Arc::new)
    .collect()
}

#[test]
fn selected_builder_matches_fold_and_restores_only_its_bound_cache() {
    let builder = selected("test.context", "1.0.0", &"a".repeat(64));
    let limits = ContextLimits::default();
    let history = complete_facts();
    let mut state = ModelContextState::open(builder.clone(), header("system"), limits).unwrap();
    state.ingest(ContextPage::Canonical(&history)).unwrap();
    let mut fold = ContextFold::with_limits(header("system"), limits).unwrap();
    fold.apply(&history).unwrap();
    assert_eq!(
        state.build(Vec::new()).unwrap(),
        fold.request(limits, Vec::new()).unwrap()
    );
    let checkpoint = state.checkpoint().unwrap();
    let mut restored = ModelContextState::open(builder.clone(), header("system"), limits).unwrap();
    restored.restore(&checkpoint).unwrap();
    assert_eq!(restored.position(), state.position());
    assert_eq!(
        restored.build(Vec::new()).unwrap(),
        state.build(Vec::new()).unwrap()
    );
    assert_eq!(builder.opened.load(Ordering::SeqCst), 3);

    for other in [
        selected("other.context", "1.0.0", &"a".repeat(64)),
        selected("test.context", "2.0.0", &"a".repeat(64)),
        selected("test.context", "1.0.0", &"b".repeat(64)),
    ] {
        let mut other_state =
            ModelContextState::open(other.clone(), header("system"), limits).unwrap();
        assert!(other_state.restore(&checkpoint).is_err());
        assert_eq!(
            other.opened.load(Ordering::SeqCst),
            1,
            "mismatched bytes reached provider"
        );
        assert_eq!(other_state.position().through_seq, 0);
    }
    for (header, limits) in [
        (header("changed"), limits),
        (
            header("system"),
            ContextLimits::new(17, limits.max_bytes).unwrap(),
        ),
    ] {
        let mut wrong = ModelContextState::open(builder.clone(), header, limits).unwrap();
        assert!(wrong.restore(&checkpoint).is_err());
    }
    let mut corrupt = checkpoint.to_vec();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    assert!(restored.restore(&corrupt).is_err());
    assert_eq!(
        restored.position(),
        state.position(),
        "failed restore replaced live cursor"
    );
    assert!(
        restored.restore(&fold.checkpoint_bytes().unwrap()).is_err(),
        "old unbound cache admitted"
    );
    assert!(
        restored
            .restore(&vec![
                0;
                rsi_agent_context::MAXIMUM_CONTEXT_CHECKPOINT_BYTES + 1
            ])
            .is_err()
    );
}

#[test]
fn selected_cursor_retains_claim_holes_and_fork_seed_ownership() {
    let builder = Arc::new(DefaultContextBuilder::default());
    let history = complete_facts();
    let mut child = ModelContextState::open(
        builder.clone(),
        fork_header("system", 2, 1),
        ContextLimits::default(),
    )
    .unwrap();
    child.ingest(ContextPage::ForkSeed(&history[..1])).unwrap();
    assert!(child.ingest(ContextPage::FinishSeed).is_err());
    child.ingest(ContextPage::ForkSeed(&history[1..])).unwrap();
    child.ingest(ContextPage::FinishSeed).unwrap();
    assert_eq!(
        child.position().through_seq,
        0,
        "seed advanced child prefix"
    );
    assert!(
        serde_json::to_string(&child.build(Vec::new()).unwrap())
            .unwrap()
            .contains("retained input")
    );

    let mut claim =
        ModelContextState::open(builder, header("system"), ContextLimits::default()).unwrap();
    claim
        .ingest(ContextPage::ClaimVisible {
            facts: &history,
            through_seq: 3,
        })
        .unwrap();
    assert_eq!(claim.position().through_seq, 3);
    assert!(
        claim.checkpoint().is_err(),
        "filtered scan became canonical checkpoint"
    );
}
