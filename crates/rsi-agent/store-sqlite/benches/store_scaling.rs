#![allow(clippy::cast_precision_loss)] // Benchmark reports convert integral nanoseconds to f64.

use rsi_agent_session_protocol::{
    AgentPresetId, EffectId, FrozenAgentSettings, SessionFact, SessionFactBody, SessionHeader,
    SessionId, TurnId, TurnOutcome,
};
use rsi_agent_store_protocol::{AppendBatch, SessionStore};
use rsi_agent_store_sqlite::SqliteStore;
use rsi_ai_protocol::{ContentDelta, LanguageEvent, ModelRef};
use rsi_sandbox::SandboxMode;
use std::future::Future;
use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, Instant};

const SAMPLES: usize = 5;

#[derive(Clone, Copy, Debug)]
enum Shape {
    LongOpen,
    MixedSessions,
}

fn main() {
    // `cargo test --all-targets` compiles harness-free benches in the test profile. Measurements
    // are intentionally release-only and have no pass/fail timing threshold.
    if cfg!(debug_assertions) {
        println!("store_scaling: skipped outside an optimized cargo bench build");
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("benchmark runtime");
    runtime.block_on(run());
}

async fn run() {
    let rust = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map_or_else(
            || "unknown".to_owned(),
            |output| String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        );
    println!("rust={rust} sqlite={}", rusqlite::version());
    let counts = std::env::var("RSI_STORE_BENCH_COUNTS").ok().map_or_else(
        || vec![1_000, 10_000, 100_000],
        |value| parse_counts(&value),
    );
    let include_million = std::env::var_os("RSI_STORE_BENCH_INCLUDE_MILLION").is_some();
    let large_payload = std::env::var_os("RSI_STORE_BENCH_LARGE_PAYLOAD").is_some();
    let mut counts = counts;
    if include_million && !counts.contains(&1_000_000) {
        counts.push(1_000_000);
    }

    for fact_count in counts {
        for payload_bytes in [256, 16 * 1024] {
            if payload_bytes > 256 && fact_count > 10_000 && !large_payload {
                println!(
                    "case facts={fact_count} payload={payload_bytes}: skipped; set \
                     RSI_STORE_BENCH_LARGE_PAYLOAD=1 for the large disk matrix"
                );
                continue;
            }
            for shape in [Shape::LongOpen, Shape::MixedSessions] {
                benchmark_case(fact_count, payload_bytes, shape).await;
            }
        }
    }
}

fn parse_counts(value: &str) -> Vec<usize> {
    let counts = value
        .split(',')
        .map(|part| part.trim().parse::<usize>().expect("positive fact count"))
        .filter(|count| *count >= 2)
        .collect::<Vec<_>>();
    assert!(!counts.is_empty(), "at least one benchmark count");
    counts
}

async fn benchmark_case(fact_count: usize, payload_bytes: usize, shape: Shape) {
    let root = tempfile::tempdir().expect("benchmark root");
    populate(root.path(), fact_count, payload_bytes, shape).await;
    let database = root.path().join("sessions.sqlite3");
    let wal = root.path().join("sessions.sqlite3-wal");
    let db_bytes = file_len(&database);
    let wal_bytes = file_len(&wal);
    let session = SessionId::new("session-000").unwrap();

    let fast_open = sample(SAMPLES, || {
        let store = SqliteStore::open(root.path()).expect("fast open");
        black_box(&store);
        drop(store);
    });
    let mut first_validation = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let store = SqliteStore::open(root.path()).expect("validation open");
        first_validation.push(
            timed_async(async {
                black_box(store.header(&session).await.expect("first validation"));
            })
            .await,
        );
    }
    let verify = sample(3, || {
        SqliteStore::verify(root.path()).expect("full verify");
    });

    let copied = tempfile::tempdir().expect("copied benchmark root");
    std::fs::copy(&database, copied.path().join("sessions.sqlite3")).expect("copy database");
    let cold_open = timed(|| drop(SqliteStore::open(copied.path()).expect("copied open")));
    let copied_store = SqliteStore::open(copied.path()).expect("copied validation open");
    let cold_validation = timed_async(async {
        black_box(
            copied_store
                .header(&session)
                .await
                .expect("copied first validation"),
        );
    })
    .await;
    drop(copied_store);
    let cold_verify = timed(|| SqliteStore::verify(copied.path()).expect("copied verify"));

    println!(
        "case facts={fact_count} payload={payload_bytes} shape={shape:?} db_bytes={db_bytes} \
         wal_bytes={wal_bytes}"
    );
    report("warm_fast_open", &fast_open);
    report("warm_first_session_validation", &first_validation);
    report("warm_full_verify", &verify);
    println!(
        "cold_new_inode open_ns={} first_validation_ns={} verify_ns={}",
        cold_open.as_nanos(),
        cold_validation.as_nanos(),
        cold_verify.as_nanos()
    );
}

async fn populate(root: &Path, fact_count: usize, payload_bytes: usize, shape: Shape) {
    let store = SqliteStore::open(root).expect("populate open");
    let sessions = match shape {
        Shape::LongOpen => 1,
        Shape::MixedSessions => 32.min(fact_count / 2),
    };
    let base = fact_count / sessions;
    let remainder = fact_count % sessions;
    for index in 0..sessions {
        let count = base + usize::from(index < remainder);
        let closed = matches!(shape, Shape::MixedSessions) && index % 2 == 0;
        append_session(&store, index, count, payload_bytes, closed).await;
    }
    drop(store);
}

async fn append_session(
    store: &SqliteStore,
    index: usize,
    fact_count: usize,
    payload_bytes: usize,
    closed: bool,
) {
    let session = SessionId::new(format!("session-{index:03}")).unwrap();
    let turn = TurnId::new(format!("turn-{index:03}")).unwrap();
    let effect = EffectId::new(format!("effect-{index:03}")).unwrap();
    let payload = "x".repeat(payload_bytes);
    let mut next_seq = 1_u64;
    let mut remaining = fact_count;
    let mut first = true;
    while remaining != 0 {
        let batch_len = remaining.min(rsi_agent_store_protocol::MAXIMUM_STORE_BATCH_FACTS);
        let mut facts = Vec::with_capacity(batch_len);
        for _ in 0..batch_len {
            let terminal = closed && remaining == 1;
            let body = if next_seq == 1 {
                SessionFactBody::TurnAccepted {
                    turn_id: turn.clone(),
                    text: "benchmark".to_owned(),
                    model: None,
                    sandbox: SandboxMode::WorkspaceWrite,
                    require_approval: false,
                }
            } else if terminal {
                SessionFactBody::TurnTerminal {
                    turn_id: turn.clone(),
                    outcome: TurnOutcome::Completed,
                }
            } else {
                SessionFactBody::ModelEvent {
                    turn_id: turn.clone(),
                    effect_id: effect.clone(),
                    event: LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Text(payload.clone()),
                    },
                }
            };
            facts.push(SessionFact::new(next_seq, next_seq, body).unwrap());
            next_seq += 1;
            remaining -= 1;
        }
        store
            .append(AppendBatch {
                session_id: session.clone(),
                expected_seq: next_seq - 1 - u64::try_from(facts.len()).unwrap(),
                header: first.then(|| header(session.clone())),
                facts,
            })
            .await
            .expect("populate append");
        first = false;
    }
}

fn header(session_id: SessionId) -> SessionHeader {
    SessionHeader::new(
        session_id,
        1,
        "/workspace",
        AgentPresetId::new("benchmark-agent").unwrap(),
        FrozenAgentSettings::new(
            "benchmark",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn timed(operation: impl FnOnce()) -> Duration {
    let started = Instant::now();
    operation();
    started.elapsed()
}

async fn timed_async(operation: impl Future<Output = ()>) -> Duration {
    let started = Instant::now();
    operation.await;
    started.elapsed()
}

fn sample(mut count: usize, mut operation: impl FnMut()) -> Vec<Duration> {
    let mut samples = Vec::with_capacity(count);
    while count != 0 {
        samples.push(timed(&mut operation));
        count -= 1;
    }
    samples
}

fn report(label: &str, samples: &[Duration]) {
    let mut nanos = samples.iter().map(Duration::as_nanos).collect::<Vec<_>>();
    nanos.sort_unstable();
    let median = nanos[(nanos.len() - 1) / 2];
    let p95 = nanos[nanos.len().saturating_mul(95).div_ceil(100) - 1];
    println!(
        "{label} min_ns={} median_ns={} p95_ns={}",
        nanos[0], median, p95
    );
}
