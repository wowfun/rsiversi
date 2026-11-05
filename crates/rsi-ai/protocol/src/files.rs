use rsi_media_protocol::FileRef;

/// Model-facing handle for a verbatim file; a path is a location, never authority.
#[expect(
    clippy::missing_panics_doc,
    reason = "Serializing a UTF-8 str to a JSON String cannot fail."
)]
pub fn file_handle_text(file: &FileRef, path: Option<&str>) -> String {
    let name = serde_json::to_string(file.name()).expect("UTF-8 file name");
    let identity = format!(
        "File {name} ({} bytes, sha256:{})",
        file.bytes(),
        &file.id().as_str()[..8]
    );
    match path {
        Some(path) => {
            let path = serde_json::to_string(path).expect("UTF-8 file path");
            format!(
                "[{identity}: verbatim read-only copy saved at {path}. Read that path with your file tools when its contents are needed; copy it to a writable location before modifying it. When delegating file work, include this saved path; only agents sharing this execution environment can read it.]"
            )
        }
        None => format!(
            "[{identity} was uploaded, but the current execution environment has no readable path. Report that limitation if its contents are needed; do not claim to have read it.]"
        ),
    }
}
