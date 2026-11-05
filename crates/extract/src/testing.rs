//! Conformance suite every [`Extractor`] implementation runs in its own tests.

use bytes::Bytes;

use crate::*;

fn file(content_type: &str, filename: Option<&str>, content: impl Into<Bytes>) -> File {
    File {
        content_type: content_type.into(),
        filename: filename.map(str::to_owned),
        content: content.into(),
    }
}

/// Checks that `extractor` keeps the [`Extractor`] contract with plain text, which every
/// implementation must read. Panics on the first violation. Needs no network.
pub async fn conformance(extractor: &dyn Extractor) {
    let text = extractor
        .extract(&file(
            "text/plain",
            Some("notes.txt"),
            "Quarterly figures for the lighthouse project",
        ))
        .await
        .expect("plain text is read")
        .expect("plain text is supported");
    assert!(
        text.contains("lighthouse"),
        "plain text lost its words: {text:?}"
    );

    let by_name = extractor
        .extract(&file(
            "application/octet-stream",
            Some("minutes.txt"),
            "Minutes of the harbour meeting",
        ))
        .await
        .expect("a generic type with a .txt name is read");
    assert!(
        by_name.is_some_and(|t| t.contains("harbour")),
        "the file name's extension was not used for a generic type"
    );

    assert!(
        extractor
            .extract(&file(
                "application/x-holtmail-unknown",
                Some("data.unknown"),
                vec![0u8, 1, 2, 3],
            ))
            .await
            .expect("an unknown type is not an error")
            .is_none(),
        "an unknown type gave text"
    );

    let long = "word ".repeat(MAX_TEXT / 5 + 1000) + "é";
    let cut = extractor
        .extract(&file("text/plain", None, long))
        .await
        .unwrap()
        .unwrap();
    assert!(cut.len() <= MAX_TEXT, "text longer than MAX_TEXT");

    let huge = vec![b'a'; MAX_INPUT + 1];
    assert!(
        matches!(
            extractor.extract(&file("text/plain", None, huge)).await,
            Err(ExtractError::TooLarge)
        ),
        "a file over MAX_INPUT was not refused"
    );

    let broken = file("text/plain", None, vec![b'o', b'k', 0xff, 0xfe, b' ', b'x']);
    let first = extractor.extract(&broken).await.unwrap();
    let again = extractor.extract(&broken).await.unwrap();
    assert_eq!(first, again, "the same file gave different answers");

    for content_type in [
        "application/pdf",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "application/vnd.oasis.opendocument.text",
    ] {
        let result = extractor
            .extract(&file(
                content_type,
                None,
                b"not really that format".to_vec(),
            ))
            .await;
        assert!(
            !matches!(result, Ok(Some(_))),
            "garbage claiming to be {content_type} gave text"
        );
    }
}
