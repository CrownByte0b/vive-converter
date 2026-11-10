//! What the ViveNotes app gets back from a conversion.
//!
//! The JNI glue in `android.rs` only moves strings across. Everything it calls is here so it can
//! be tested on the host.

use crate::{Identity, InputError, convert_file};
use serde_json::{Value, json};
use std::path::Path;

/// Converts `input` into a `.vive` at `output`, with ids from `seed`, and describes the result.
///
/// Success is `{sections, pages, strokes, attachments, warnings}`. Failure is `{error, detail}`:
/// `error` is one of `tableOfContents`, `unreadable`, `noSections` or `failed`, and is what the
/// app words its message from. `detail` is the whole cause, for the log.
pub(crate) fn convert_to_bundle(input: &Path, file_name: &str, seed: &str, output: &Path) -> Value {
    let converted = convert_file(input, file_name, Identity::Seeded(seed)).and_then(|conversion| {
        std::fs::write(output, &conversion.bytes)?;
        Ok(conversion)
    });
    match converted {
        Ok(conversion) => json!({
            "sections": conversion.summary.sections,
            "pages": conversion.summary.pages,
            "strokes": conversion.summary.strokes,
            "attachments": conversion.summary.attachments,
            "warnings": conversion.warnings,
        }),
        Err(error) => failure(
            match error.downcast_ref::<InputError>() {
                Some(InputError::TableOfContents) => "tableOfContents",
                Some(InputError::Unreadable) => "unreadable",
                Some(InputError::NoSections) => "noSections",
                None => "failed",
            },
            &format!("{error:#}"),
        ),
    }
}

pub(crate) fn failure(code: &str, detail: &str) -> Value {
    json!({ "error": code, "detail": detail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{notebook_id, package, sample};

    #[test]
    fn writes_the_bundle_and_counts_it() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("incoming");
        let output = directory.path().join("converted.vive");
        std::fs::write(&input, package()).unwrap();

        let report = convert_to_bundle(&input, "Group.onepkg", "seed", &output);

        assert_eq!(report["sections"], 2, "{report}");
        assert!(report["warnings"].is_array());
        assert!(report.get("error").is_none());
        let bundle = std::fs::read(&output).unwrap();
        assert!(bundle.starts_with(b"PK\x03\x04"));
        assert!(!notebook_id(&bundle).is_empty());
    }

    #[test]
    fn names_what_went_wrong() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("converted.vive");
        let cases = [
            (
                sample("Open Notebook.onetoc2"),
                "Open Notebook.onetoc2",
                "tableOfContents",
            ),
            (b"%PDF-1.7".to_vec(), "paper.pdf", "unreadable"),
        ];
        for (bytes, name, code) in cases {
            let input = directory.path().join("incoming");
            std::fs::write(&input, bytes).unwrap();
            let report = convert_to_bundle(&input, name, "seed", &output);
            assert_eq!(report["error"], code, "{report}");
            assert!(
                report["detail"]
                    .as_str()
                    .is_some_and(|detail| !detail.is_empty())
            );
        }
        assert!(!output.exists());

        let missing =
            convert_to_bundle(&directory.path().join("missing"), "a.one", "seed", &output);
        // Not the user's file at fault: the app lost its own staged copy.
        assert_eq!(missing["error"], "failed", "{missing}");
    }
}
