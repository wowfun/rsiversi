use super::*;
fn body(media: &str, bytes: impl Into<Vec<u8>>) -> Body {
    Body {
        url: url::Url::parse("https://example.com/").unwrap(),
        media: media.into(),
        encoding: String::new(),
        bytes: bytes.into(),
    }
}
#[test]
fn html_is_external_text_with_entities_title_and_no_executable_or_hidden_content() {
    let actual = extract(&body("text/html; charset=utf-8",br"<!doctype html><title>A &amp; B</title><script>fake <p>secret</p></script><style>hidden</style><noscript>hidden</noscript><template><template>hidden</template>hidden</template><h1>Hello &lt;world&gt;</h1><p>one <b>two</b></p><p>three</p>".to_vec()),&CancellationToken::new()).unwrap();
    assert_eq!(actual.title, "A & B");
    assert_eq!(actual.text, "Hello <world> one two three");
    assert!(!actual.truncated);
}
#[test]
fn charset_bom_and_invalid_decoding_have_explicit_results() {
    assert_eq!(
        extract(
            &body("text/plain; charset=windows-1252", b"caf\xe9".to_vec()),
            &CancellationToken::new()
        )
        .unwrap()
        .text,
        "caf\u{e9}"
    );
    assert_eq!(
        extract(
            &body("text/plain", vec![0xff, 0xfe, b'x', 0]),
            &CancellationToken::new()
        )
        .unwrap()
        .text,
        "x"
    );
    assert!(matches!(
        extract(&body("text/plain", vec![0xff]), &CancellationToken::new()),
        Err(Error::Decode)
    ));
    assert!(matches!(
        extract(
            &body("application/pdf", b"pdf".to_vec()),
            &CancellationToken::new()
        ),
        Err(Error::UnsupportedContent)
    ));
    assert!(matches!(
        extract(
            &body("text/plain; charset=unknown-encoding", b"x".to_vec()),
            &CancellationToken::new()
        ),
        Err(Error::UnsupportedContent)
    ));
    let mut value = body("text/plain", b"x".to_vec());
    value.encoding = "compress".into();
    assert!(matches!(
        extract(&value, &CancellationToken::new()),
        Err(Error::UnsupportedContent)
    ));
}
#[test]
fn text_truncation_preserves_utf8_but_oversize_decoding_and_pending_tokens_fail() {
    let source = "界".repeat(MAX_TEXT / 3 + 2);
    let result = extract(
        &body("text/plain", source.into_bytes()),
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(result.truncated);
    assert_eq!(result.text.len(), MAX_TEXT / 3 * 3);
    assert!(matches!(
        extract(
            &body("text/plain", vec![b'x'; MAX_DECODED + 1]),
            &CancellationToken::new()
        ),
        Err(Error::Capacity)
    ));
    assert!(matches!(
        extract(
            &body(
                "text/html",
                format!("<a huge='{}'>", "x".repeat(100 * 1024)).into_bytes()
            ),
            &CancellationToken::new()
        ),
        Err(Error::Capacity)
    ));
    assert!(matches!(
        extract(
            &body(
                "text/html",
                format!("<!--{}-->", "x".repeat(100 * 1024)).into_bytes()
            ),
            &CancellationToken::new()
        ),
        Err(Error::Capacity)
    ));
    assert!(matches!(
        extract(
            &body("text/html", "<template>".repeat(257).into_bytes()),
            &CancellationToken::new()
        ),
        Err(Error::Capacity)
    ));
}
#[test]
fn gzip_brotli_expansion_is_bounded_and_cancellation_is_checked_during_decode() {
    let bytes = vec![b'x'; MAX_DECODED + 1];
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gzip, &bytes).unwrap();
    let mut value = body("text/plain", gzip.finish().unwrap());
    value.encoding = "gzip".into();
    assert!(value.bytes.len() < 1024 * 1024);
    assert!(matches!(
        extract(&value, &CancellationToken::new()),
        Err(Error::Capacity)
    ));
    let mut compressed = vec![];
    {
        let mut writer = brotli::CompressorWriter::new(&mut compressed, 4096, 1, 20);
        std::io::Write::write_all(&mut writer, b"Brotli text").unwrap();
    }
    let mut value = body("text/plain", compressed);
    value.encoding = "br".into();
    assert_eq!(
        extract(&value, &CancellationToken::new()).unwrap().text,
        "Brotli text"
    );
    let stop = CancellationToken::new();
    stop.cancel();
    assert!(matches!(extract(&value, &stop), Err(Error::Cancelled)));
}
