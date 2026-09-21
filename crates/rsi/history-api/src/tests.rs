use super::*;
use serde_json::json;

fn fixture() -> (Request, Hit, Coverage) {
    let scope = serde_json::from_value(
        json!({"workspace":"b".repeat(64),"conversation":{"kind":"external","id":"history"}}),
    )
    .unwrap();
    let hit: Hit = serde_json::from_value(json!({"source":{"kind":"observed","owner":"acp","id":"history","epoch":"1"},"original":{"record":{"sequence":"1","kind":"human","content_index":0},"through_seq":"2","start":0,"end":4,"text_sha256":"a".repeat(64),"scanned_bytes":128},"preview":"text"})).unwrap();
    let coverage = Coverage {
        source: hit.source.clone(),
        indexed_through: "2".into(),
        observed_through: "2".into(),
        omissions: "0".into(),
        has_more: false,
    };
    (
        Request::Search {
            scope,
            query: "text".into(),
            after: None,
        },
        hit,
        coverage,
    )
}

#[test]
fn history_client_rejects_uncorrelated_duplicate_or_unbounded_candidates() {
    let (request, hit, coverage) = fixture();
    let mut reply = Reply::Hits {
        coverage,
        hits: vec![hit.clone()],
        next: None,
    };
    validate_reply(&request, &reply).unwrap();
    if let Reply::Hits { hits, .. } = &mut reply {
        hits.push(hit);
    }
    assert!(validate_reply(&request, &reply).is_err());
    if let Reply::Hits { hits, coverage, .. } = &mut reply {
        hits.pop();
        coverage.observed_through = "3".into();
    }
    assert!(
        validate_reply(&request, &reply).is_err(),
        "unindexed tail cannot claim full coverage"
    );
    if let Reply::Hits { coverage, .. } = &mut reply {
        coverage.has_more = true;
        coverage.source = ReferenceSource::Observed {
            owner: "acp".into(),
            id: "foreign".into(),
            epoch: 1,
        };
    }
    assert!(validate_reply(&request, &reply).is_err());
}

#[test]
fn history_client_rejects_backward_original_windows_and_repeated_cursors() {
    let (request, hit, coverage) = fixture();
    let Request::Search { scope, query, .. } = request else {
        unreachable!()
    };
    let read = Request::Read {
        scope: scope.clone(),
        hit: hit.clone(),
        offset: 4,
    };
    let backward = Reply::Original {
        hit: hit.clone(),
        offset: 4,
        next_offset: 0,
        has_more: true,
        text: String::new(),
    };
    assert!(validate_reply(&read, &backward).is_err());
    let cursor = Cursor {
        generation: "a".repeat(32),
        query: query.clone(),
        scope: scope.clone(),
        after: "7".into(),
    };
    let search = Request::Search {
        scope,
        query,
        after: Some(cursor.clone()),
    };
    let reply = Reply::Hits {
        coverage,
        hits: vec![hit],
        next: Some(cursor),
    };
    assert!(validate_reply(&search, &reply).is_err());
}
