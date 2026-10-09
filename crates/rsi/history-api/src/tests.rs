use super::*;
use serde_json::json;

#[test]
fn query_byte_and_continuation_bounds_reject_before_source_work() {
    let request = |query: String| Request::Query {
        scope: QueryScope::AccessibleHost,
        query,
        after: None,
    };
    assert!(request("界".repeat(85)).validate().is_ok());
    assert!(request("界".repeat(86)).validate().is_err());
    assert!(request("a".repeat(256)).validate().is_ok());
    for query in ["a".repeat(257), " ".into(), "a\nb".into()] {
        assert!(request(query).validate().is_err());
    }
    assert!(
        Request::Discover {
            scope: QueryScope::AccessibleHost,
            after: Some("a".repeat(32))
        }
        .validate()
        .is_ok()
    );
    for after in ["a".repeat(31), "A".repeat(32), "a".repeat(33)] {
        assert!(
            Request::Discover {
                scope: QueryScope::AccessibleHost,
                after: Some(after)
            }
            .validate()
            .is_err()
        );
    }
}

#[test]
fn replies_reject_hit_count_and_encoded_byte_overflow() {
    let (request, hit, coverage) = fixture();
    let hits = (0..64)
        .map(|sequence| {
            let mut hit = hit.clone();
            hit.original.record.sequence = sequence + 1;
            hit.original.through_seq = 65;
            hit
        })
        .collect::<Vec<_>>();
    let mut reply = Reply::Hits {
        coverage,
        hits,
        next: None,
    };
    // Ensure the count probe uses distinct, valid records within the observed horizon.
    if let Reply::Hits { coverage, .. } = &mut reply {
        coverage.indexed_through = "65".into();
        coverage.observed_through = "65".into();
    }
    validate_reply(&request, &reply).unwrap();
    if let Reply::Hits { hits, .. } = &mut reply {
        let mut extra = hit;
        extra.original.record.sequence = 65;
        extra.original.through_seq = 65;
        hits.push(extra);
    }
    assert!(validate_reply(&request, &reply).is_err());
    if let Reply::Hits { hits, .. } = &mut reply {
        hits.pop();
        for hit in hits {
            hit.original.end = 2048;
            hit.original.scanned_bytes = 4096;
            hit.preview = "\u{0001}".repeat(2048);
            hit.validate().unwrap();
        }
    }
    assert!(serde_json::to_vec(&reply).unwrap().len() > 256 * 1024);
    assert!(validate_reply(&request, &reply).is_err());
}
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

#[test]
fn range_reply_checks_source_membership_and_opaque_progress_without_raw_scan_counts() {
    let (exact, hit, coverage) = fixture();
    let scope = exact.scope().unwrap().clone();
    let request = Request::Query {
        scope: QueryScope::Workspace {
            workspace: scope.workspace.clone(),
        },
        query: "text".into(),
        after: None,
    };
    let mut reply = Reply::Matches {
        progress: QueryProgress {
            visible_sources: 1,
            ..Default::default()
        },
        matches: vec![Match {
            label: "saved project".into(),
            scope,
            hit,
            reference_allowed: true,
        }],
        next: None,
    };
    validate_reply(&request, &reply).unwrap();
    if let Reply::Matches { matches, .. } = &mut reply {
        matches[0].scope.workspace = serde_json::from_value(json!("c".repeat(64))).unwrap();
    }
    assert!(validate_reply(&request, &reply).is_err());
    let progress = Request::Progress {
        scope: QueryScope::AccessibleHost,
        after: None,
    };
    let source = SourceCoverage {
        label: "saved project".into(),
        unavailable: false,
        scope: exact.scope().unwrap().clone(),
        coverage,
        reference_allowed: true,
    };
    let reply = Reply::Progress {
        progress: QueryProgress {
            continuation: Some("a".repeat(32)),
            ..Default::default()
        },
        sources: vec![source],
        next: None,
    };
    validate_reply(&progress, &reply).unwrap();
    let encoded = serde_json::to_value(&reply).unwrap();
    assert!(encoded["progress"].get("sources").is_none());
    assert!(encoded["progress"].get("native_after").is_none());
}

#[test]
fn partial_metadata_failure_cannot_claim_complete_discovery() {
    let request = Request::Progress {
        scope: QueryScope::AccessibleHost,
        after: None,
    };
    let mut reply = Reply::Progress {
        progress: QueryProgress {
            metadata_unavailable: true,
            ..Default::default()
        },
        sources: vec![],
        next: None,
    };
    validate_reply(&request, &reply).unwrap();
    if let Reply::Progress { progress, .. } = &mut reply {
        progress.discovery_complete = true;
    }
    assert!(validate_reply(&request, &reply).is_err());
}
