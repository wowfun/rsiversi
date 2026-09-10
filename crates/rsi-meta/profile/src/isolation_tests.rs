use super::*;

#[test]
fn compiled_fanout_and_candidate_copies_share_immutable_isolation_lane_storage() {
    let isolation = IsolationSpec::new(["local".into()], ["event".into()], ["portable".into()])
        .with_named(IsolationLane::Local, "named", "one");
    let changed = isolation
        .clone()
        .with_named(IsolationLane::Local, "another", "two");
    assert_eq!(isolation.named().len(), 1);
    assert_eq!(changed.named().len(), 2);
    let program = ProfileProgram::from_profile(Profile::default()).with_linked_fragments(vec![
        ProfileFragment::program(
            "shared",
            [ProfileStep::Node(ProfileNode::Group(
                ProfileGroup::new(
                    "g",
                    (0..64).map(|id| {
                        ProfileNode::Plugin(ProfileEntry::new(
                            format!("leaf-{id}"),
                            "noop",
                            Value::Null,
                        ))
                    }),
                )
                .isolation(isolation.clone()),
            ))],
        ),
    ]);
    let candidate = ProfileCompiler::new(
        ProfileEnvironment::without_paths("test", BTreeMap::new()).unwrap(),
        ProfileLimits::default(),
    )
    .compile(&program)
    .unwrap();
    for snapshot in [candidate.clone(), candidate] {
        for leaf in snapshot.leaves() {
            let bound = &leaf.isolations()[0];
            assert!(Arc::ptr_eq(&isolation.local, &bound.local));
            assert!(Arc::ptr_eq(&isolation.events, &bound.events));
            assert!(Arc::ptr_eq(&isolation.portable, &bound.portable));
            assert!(Arc::ptr_eq(&isolation.named, &bound.named));
        }
    }
}
