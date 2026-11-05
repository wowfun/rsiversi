use super::*;
use rsi_pty_protocol::Size;
use rsi_sandbox::{
    ConfinedProcess, EnforcementStamp, ProcessStdio, SandboxBackend, SandboxFileSystem,
    SandboxMode, SandboxNetwork, SandboxScratch,
};

fn launch(size: Size) -> TerminalLaunch {
    let workspace = std::env::current_dir().unwrap();
    TerminalLaunch::Native(rsi_process::PtyProcessSpec {
        process: ConfinedProcess {
            program: workspace.join("fixture"),
            owner: None,
            stdio: ProcessStdio::Pty,
            arguments: vec![],
            cwd: workspace.clone(),
            stamp: EnforcementStamp {
                requested: SandboxMode::DangerFullAccess,
                backend: SandboxBackend::Unconfined,
                filesystem: SandboxFileSystem::Unconfined,
                scratch: SandboxScratch::Host,
                network: SandboxNetwork::Host,
                workspace,
            },
        },
        environment: vec![],
        size: size.native(),
        termination_grace_ms: 250,
    })
}

#[tokio::test]
async fn close_during_allocation_rejects_original_duplicate_and_late_creates() {
    let provider = Scope::new(false, false);
    let registry = registry(provider.clone(), Scope::new(false, false));
    let key = (
        SessionId::new("first").unwrap(),
        None,
        String::from("pending"),
    );
    let size = Size {
        rows: 24,
        columns: 80,
    };
    let create = || registry.create_keyed(key.clone(), size, launch(size));
    let original = create();
    let duplicate = create();
    let close = registry.close_keyed(key.clone());
    tokio::pin!(original, duplicate, close);
    // Drive each request up to its first wait in this exact order.
    for pending in [&mut original, &mut duplicate] {
        tokio::select! { biased;
            result = pending => panic!("create returned before allocation: {result:?}"),
            () = std::future::ready(()) => {},
        }
    }
    tokio::select! { biased;
        result = &mut close => panic!("close returned before cleanup: {result:?}"),
        () = std::future::ready(()) => {},
    }
    let unrelated = (key.0.clone(), None, String::from("unseen"));
    tokio::select! { biased;
        result = registry.close_keyed(unrelated) => assert_eq!(result.unwrap(), Reply::Done),
        () = std::future::ready(()) => panic!("unrelated allocation delayed close"),
    }
    tokio::select! { biased;
        result = create() => assert!(matches!(result, Err(SessionError::TerminalKeyRetired))),
        () = std::future::ready(()) => panic!("late create waited for an already closed key"),
    }
    provider.release.notify_one();
    let (first, second, closed) = tokio::join!(original, duplicate, close);
    assert!(matches!(first, Err(SessionError::TerminalKeyRetired)));
    assert!(matches!(second, Err(SessionError::TerminalKeyRetired)));
    assert_eq!(closed.unwrap(), Reply::Done);
    assert!(provider.is_empty());
    assert!(matches!(
        create().await,
        Err(SessionError::TerminalKeyRetired)
    ));
}
