use rsi_conversation::{
    FactField, MAXIMUM_BLOCK_SOURCES, SourceAdmission, SourceIndex, SourceIndexError, SourceRef,
};

fn source(seq: u64) -> SourceRef {
    SourceRef {
        seq,
        field: FactField::ModelText,
    }
}

#[test]
fn exact_sources_admit_missing_interior_and_preserve_mixed_content_order() {
    let mut index = SourceIndex::default();
    assert_eq!(
        index.insert(source(0)),
        Err(SourceIndexError::InvalidSource)
    );
    assert!(index.is_empty());
    assert!(serde_json::to_string(&source(0)).is_err());
    assert_eq!(
        index.insert(source(3)).unwrap(),
        SourceAdmission::Inserted(0)
    );
    assert_eq!(
        index.insert(source(1)).unwrap(),
        SourceAdmission::Inserted(0)
    );
    assert_eq!(
        index.insert(source(2)).unwrap(),
        SourceAdmission::Inserted(1)
    );
    assert_eq!(
        index.insert(source(2)).unwrap(),
        SourceAdmission::Existing(1)
    );
    assert_eq!(
        index.iter().collect::<Vec<_>>(),
        [source(1), source(2), source(3)]
    );
    for (kind, expected) in [
        (
            true,
            vec![
                FactField::InputText { index: 0 },
                FactField::InputImage { index: 1 },
                FactField::InputText { index: 2 },
            ],
        ),
        (
            false,
            vec![
                FactField::ToolText { index: 0 },
                FactField::ToolImage { index: 1 },
                FactField::ToolText { index: 2 },
            ],
        ),
    ] {
        let mut mixed = SourceIndex::default();
        for field in expected.iter().rev() {
            mixed
                .insert(SourceRef {
                    seq: 7,
                    field: *field,
                })
                .unwrap();
        }
        assert_eq!(
            mixed.iter().map(|source| source.field).collect::<Vec<_>>(),
            expected,
            "input={kind}"
        );
    }
    let forgotten = index.remove(1).unwrap();
    assert_eq!(index.position(forgotten), None);
    assert_eq!(
        index.insert(forgotten).unwrap(),
        SourceAdmission::Inserted(1)
    );
    assert_eq!(index.get(3), None);
    assert_eq!(index.remove(3), None);
}

#[test]
fn source_index_bounds_allocation_and_lets_renderers_choose_eviction() {
    let mut index = SourceIndex::default();
    for seq in 1..=u64::try_from(MAXIMUM_BLOCK_SOURCES).unwrap() {
        index.insert(source(seq)).unwrap();
    }
    assert_eq!(index.len(), MAXIMUM_BLOCK_SOURCES);
    assert!(index.owned_bytes() <= MAXIMUM_BLOCK_SOURCES * std::mem::size_of::<SourceRef>());
    assert_eq!(
        index.insert(source(1)).unwrap(),
        SourceAdmission::Existing(0)
    );
    assert_eq!(
        index.insert(source(u64::MAX)),
        Err(SourceIndexError::Capacity)
    );
    assert_eq!(index.get(0), Some(source(1)));
    assert_eq!(index.remove(0), Some(source(1)));
    assert_eq!(
        index.insert(source(u64::MAX)).unwrap(),
        SourceAdmission::Inserted(MAXIMUM_BLOCK_SOURCES - 1)
    );
    assert_eq!(index.remove(index.len() - 1), Some(source(u64::MAX)));
    assert_eq!(
        index.insert(source(1)).unwrap(),
        SourceAdmission::Inserted(0)
    );
    assert_eq!(
        index.get(index.len() - 1),
        Some(source(u64::try_from(MAXIMUM_BLOCK_SOURCES).unwrap()))
    );
}
