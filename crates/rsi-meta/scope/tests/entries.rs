use rsi_meta_scope::{AnonymousEntries, NamedEntries};
use std::sync::{Arc, Weak};

#[test]
fn named_entries_return_owned_insertion_ordered_snapshots_and_exact_undo() {
    let entries = NamedEntries::new();
    let remove_a = entries.insert("a", 1).unwrap();
    let remove_b = entries.insert("b", 2).unwrap();

    assert_eq!(
        entries.snapshot(),
        vec![("a".to_owned(), 1), ("b".to_owned(), 2)]
    );
    assert_eq!(entries.get("a"), Some(1));
    assert!(entries.contains("b"));
    assert!(matches!(entries.insert("a", 3), Err(3)));

    remove_a.run();
    let replacement = entries.insert("a", 3).unwrap();
    remove_a.run();
    assert_eq!(
        entries.snapshot(),
        vec![("b".to_owned(), 2), ("a".to_owned(), 3)]
    );

    remove_b.run();
    replacement.run();
    assert!(entries.is_empty());
}

#[test]
fn anonymous_equal_values_keep_independent_identities() {
    let entries = AnonymousEntries::new();
    let first = entries.append("same".to_owned());
    let second = entries.append("same".to_owned());

    assert_eq!(
        entries.snapshot(),
        vec!["same".to_owned(), "same".to_owned()]
    );
    first.run();
    first.run();
    assert_eq!(entries.snapshot(), vec!["same".to_owned()]);
    second.run();
    assert!(entries.is_empty());
}

#[test]
fn snapshots_are_detached_from_later_table_mutation() {
    let named = NamedEntries::new();
    let anonymous = AnonymousEntries::new();
    let _named = named.insert("first", String::from("old")).unwrap();
    let _anonymous = anonymous.append(String::from("old"));
    let named_snapshot = named.snapshot();
    let anonymous_snapshot = anonymous.snapshot();

    let _later_named = named.insert("second", String::from("new")).unwrap();
    let _later_anonymous = anonymous.append(String::from("new"));

    assert_eq!(
        named_snapshot,
        vec![(String::from("first"), String::from("old"))]
    );
    assert_eq!(anonymous_snapshot, vec![String::from("old")]);
}

struct ReentrantClone {
    table: Weak<NamedEntries<ReentrantClone>>,
}

impl Clone for ReentrantClone {
    fn clone(&self) -> Self {
        let table = self.table.upgrade().unwrap();
        assert!(table.contains("value"));
        Self {
            table: Arc::downgrade(&table),
        }
    }
}

#[test]
fn owned_snapshot_never_clones_product_values_under_the_table_lock() {
    let entries = Arc::new(NamedEntries::new());
    let _undo = entries
        .insert(
            "value",
            ReentrantClone {
                table: Arc::downgrade(&entries),
            },
        )
        .ok()
        .unwrap();

    assert_eq!(entries.snapshot().len(), 1);
}
