use super::*;
use std::time::Instant;

fn distribution(mut samples: Vec<f64>) -> serde_json::Value {
    samples.sort_by(f64::total_cmp);
    serde_json::json!({"samples":samples.len(),"median_ms":samples[samples.len()/2],
        "p95_ms":samples[(samples.len()*95/100).min(samples.len()-1)]})
}

#[tokio::test]
#[ignore = "opt-in paired native Store read and physical-size evidence; no timing threshold"]
async fn compare_referenced_and_inline_claim_storage() {
    let output = std::env::var_os("RSI_STORE_CLAIM_BENCH_OUTPUT").expect("explicit evidence path");
    let roots = [private_tempdir(), private_tempdir()];
    let mut expected = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        let store = SqliteStore::open(root.path()).unwrap();
        let (_, facts) = super::claim_storage::seed(&store, true).await;
        expected = facts;
        drop(store);
        if index == 1 {
            let db = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
            let encoded = serde_json::to_string(&expected[2]).unwrap();
            db.execute("UPDATE facts SET fact_json=?1, acceptance_control_seq=NULL, logical_bytes=NULL WHERE seq=3", [&encoded]).unwrap();
            db.execute(
                "UPDATE reference_records SET sha256=?1 WHERE stream='fact' AND seq=3",
                [hex::encode(Sha256::digest(encoded.as_bytes()))],
            )
            .unwrap();
        }
        SqliteStore::verify(root.path()).unwrap();
        SqliteStore::compact(root.path()).unwrap();
    }
    let stores = roots
        .iter()
        .map(|root| SqliteStore::open(root.path()).unwrap())
        .collect::<Vec<_>>();
    let id = SessionId::new("claim-storage").unwrap();
    for store in &stores {
        assert_eq!(
            store
                .read_facts(&id, 0, 64, 1024 * 1024)
                .await
                .unwrap()
                .facts,
            expected
        );
    }
    let mut warm = [Vec::new(), Vec::new()];
    for round in 0..128 {
        for index in [round % 2, 1 - round % 2] {
            let start = Instant::now();
            let page = stores[index]
                .read_facts(&id, 0, 64, 1024 * 1024)
                .await
                .unwrap();
            warm[index].push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(page.facts, expected);
        }
    }
    drop(stores);
    let mut cold = [Vec::new(), Vec::new()];
    for round in 0..16 {
        for index in [round % 2, 1 - round % 2] {
            let store = SqliteStore::open(roots[index].path()).unwrap();
            let start = Instant::now();
            let page = store.read_facts(&id, 0, 64, 1024 * 1024).await.unwrap();
            cold[index].push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(page.facts, expected);
        }
    }
    let [referenced_warm, inline_warm] = warm;
    let [referenced_cold, inline_cold] = cold;
    let evidence = serde_json::json!({
        "scope":"Native Store API; paired alternating order; cold means fresh Session proof, not OS-cold pages; identical typed Facts and valid coverage",
        "logical_claim_bytes":expected[2].encoded_len(),
        "referenced":{"database_bytes":std::fs::metadata(roots[0].path().join("sessions.sqlite3")).unwrap().len(),"warm":distribution(referenced_warm),"cold":distribution(referenced_cold)},
        "inline":{"database_bytes":std::fs::metadata(roots[1].path().join("sessions.sqlite3")).unwrap().len(),"warm":distribution(inline_warm),"cold":distribution(inline_cold)}
    });
    std::fs::write(output, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    println!("{evidence}");
}
