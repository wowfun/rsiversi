#![cfg(all(target_os = "linux", feature = "test-support"))]

#[test]
fn fuzz_seeds_use_the_bounded_production_parser() {
    use rsi_apply_patch::exercise_patch_parser as parse;
    assert_eq!(parse(include_bytes!("corpus/unicode.patch")), Ok(()));
    for operation in [
        "*** Delete File: file",
        "*** Update File: file\n*** Move to: moved\n@@\n-old\n+new",
    ] {
        assert_eq!(
            parse(format!("*** Begin Patch\n{operation}\n*** End Patch\n").as_bytes()),
            Ok(())
        );
    }
    assert_eq!(
        parse(include_bytes!("corpus/traversal.patch")),
        Err("invalid_path".into())
    );
    assert_eq!(parse(&[255]), Err("invalid_utf8".into()));
    assert_eq!(
        parse(&vec![b'x'; rsi_apply_patch::MAXIMUM_APPLY_PATCH_BYTES + 1]),
        Err("patch_too_large".into())
    );
    for control in ['\0', '\u{1f}', '\u{7f}'] {
        let document =
            format!("*** Begin Patch\n*** Add File: file\n+text{control}\n*** End Patch\n");
        assert_eq!(parse(document.as_bytes()), Err("invalid_patch_text".into()));
    }
    assert_eq!(
        parse(b"*** Begin Patch\n*** Add File: file\n+text\tmore\r\n*** End Patch\n"),
        Ok(())
    );
}
