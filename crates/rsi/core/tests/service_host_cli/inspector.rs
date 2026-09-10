use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finite_inspector_uses_existing_owner_and_real_uds_without_a_session() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    let help = fixture.assert_success(&["--profile", "inspector", "--help"]);
    assert!(String::from_utf8_lossy(&help.stdout).contains("AFTER_FIBER"));
    assert!(
        !fixture
            .run(&["--profile", "inspector", "runtime"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["--profile", "inspector", "runtime", "01"])
            .status
            .success()
    );
    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    for name in ["runtime", "profile", "factories", "native"] {
        let output = fixture.assert_success(&["--profile", "inspector", name]);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(value.is_object());
        if let Some(directory) = std::env::var_os("RSI_INSPECTOR_REPORT") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(format!("{name}.json")), &output.stdout).unwrap();
        }
        assert!(
            !String::from_utf8_lossy(&output.stdout)
                .contains(fixture.temporary.path().to_str().unwrap())
        );
        if name == "runtime" {
            assert!(value["total_fibers"].as_u64().unwrap() > 0);
            if let Some(next) = value["next_after"].as_str() {
                fixture.assert_success(&["--profile", "inspector", "runtime", next]);
            }
        }
        if name == "native" {
            assert_eq!(value["health"], "ready");
        }
    }
    fixture.assert_success(&["host", "stop"]);
    assert!(
        !fixture
            .run(&["--profile", "inspector", "profile"])
            .status
            .success()
    );
    provider.abort();
}
