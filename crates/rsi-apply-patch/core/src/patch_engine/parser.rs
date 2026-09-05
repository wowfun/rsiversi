use super::{EngineFailure, MAXIMUM_PATCH_OPERATIONS, MAXIMUM_PATCH_PATH_BYTES_TOTAL, SafePath};

const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
const END_PATCH_MARKER: &str = "*** End Patch";
const ADD_FILE_MARKER: &str = "*** Add File: ";
const DELETE_FILE_MARKER: &str = "*** Delete File: ";
const UPDATE_FILE_MARKER: &str = "*** Update File: ";
const MOVE_TO_MARKER: &str = "*** Move to: ";
const END_OF_FILE_MARKER: &str = "*** End of File";

#[derive(Clone, Debug)]
pub(super) enum ParsedOperation {
    Add {
        path: SafePath,
        content: Vec<u8>,
    },
    Update {
        path: SafePath,
        move_path: Option<SafePath>,
        chunks: Vec<UpdateChunk>,
    },
    Delete {
        path: SafePath,
    },
}

#[derive(Clone, Debug)]
pub(super) struct UpdateChunk {
    pub(super) context: Option<String>,
    pub(super) old_lines: Vec<String>,
    pub(super) new_lines: Vec<String>,
    pub(super) context_indices: Vec<(usize, usize)>,
    pub(super) eof: bool,
}

pub(super) fn parse_patch(document: &str) -> Result<Vec<ParsedOperation>, EngineFailure> {
    PatchParser::new(document)?.parse()
}

struct PatchParser {
    lines: Vec<String>,
    cursor: usize,
    path_bytes: usize,
}

impl PatchParser {
    fn new(document: &str) -> Result<Self, EngineFailure> {
        let normalized = document.replace("\r\n", "\n");
        let mut lines = normalized.lines().map(str::to_owned).collect::<Vec<_>>();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        if lines.first().map(String::as_str) != Some(BEGIN_PATCH_MARKER) {
            return Err(EngineFailure::new(
                "invalid_patch",
                "first line must be '*** Begin Patch'",
            ));
        }
        if lines.last().map(String::as_str) != Some(END_PATCH_MARKER) {
            return Err(EngineFailure::new(
                "invalid_patch",
                "last line must be '*** End Patch'",
            ));
        }
        Ok(Self {
            lines,
            cursor: 1,
            path_bytes: 0,
        })
    }

    fn parse(mut self) -> Result<Vec<ParsedOperation>, EngineFailure> {
        let mut operations = Vec::new();
        while self.body_remaining() {
            if operations.len() >= MAXIMUM_PATCH_OPERATIONS {
                return Err(EngineFailure::new(
                    "too_many_operations",
                    format!("patch exceeds {MAXIMUM_PATCH_OPERATIONS} operations"),
                ));
            }
            operations.push(self.parse_operation(operations.len())?);
        }
        if operations.is_empty() {
            return Err(EngineFailure::new(
                "empty_patch",
                "patch must contain at least one operation",
            ));
        }
        Ok(operations)
    }

    fn parse_operation(&mut self, operation: usize) -> Result<ParsedOperation, EngineFailure> {
        let Some(header) = operation_header(&self.lines[self.cursor]) else {
            return Err(EngineFailure::new(
                "invalid_patch",
                format!("unknown operation header at patch line {}", self.cursor + 1),
            ));
        };
        match header {
            OperationHeader::Add(path) => {
                let path = parse_bounded_path(path, &mut self.path_bytes)?;
                self.cursor += 1;
                self.parse_add(operation, path)
            }
            OperationHeader::Delete(path) => {
                let path = parse_bounded_path(path, &mut self.path_bytes)?;
                self.cursor += 1;
                Ok(ParsedOperation::Delete { path })
            }
            OperationHeader::Update(path) => {
                let path = parse_bounded_path(path, &mut self.path_bytes)?;
                self.cursor += 1;
                self.parse_update(operation, path)
            }
        }
    }

    fn parse_add(
        &mut self,
        operation: usize,
        path: SafePath,
    ) -> Result<ParsedOperation, EngineFailure> {
        let mut content = String::new();
        while self.body_remaining() && operation_header(&self.lines[self.cursor]).is_none() {
            let Some(line) = self.lines[self.cursor].strip_prefix('+') else {
                return Err(EngineFailure::new(
                    "invalid_add_hunk",
                    format!("add operation {operation} contains a non-'+' line"),
                )
                .at(operation, &path));
            };
            content.push_str(line);
            content.push('\n');
            self.cursor += 1;
        }
        if content.is_empty() {
            return Err(EngineFailure::new(
                "invalid_add_hunk",
                "add operation must contain at least one '+' line",
            )
            .at(operation, &path));
        }
        Ok(ParsedOperation::Add {
            path,
            content: content.into_bytes(),
        })
    }

    fn parse_update(
        &mut self,
        operation: usize,
        path: SafePath,
    ) -> Result<ParsedOperation, EngineFailure> {
        let move_path = self.parse_move_path()?;
        let mut chunks = Vec::new();
        while self.body_remaining() && operation_header(&self.lines[self.cursor]).is_none() {
            chunks.push(self.parse_update_chunk(operation, &path)?);
        }
        if chunks.is_empty() {
            return Err(EngineFailure::new(
                "invalid_update_hunk",
                "update operation contains no hunks",
            )
            .at(operation, &path));
        }
        Ok(ParsedOperation::Update {
            path,
            move_path,
            chunks,
        })
    }

    fn parse_move_path(&mut self) -> Result<Option<SafePath>, EngineFailure> {
        if !self.body_remaining() {
            return Ok(None);
        }
        let Some(destination) = self.lines[self.cursor].strip_prefix(MOVE_TO_MARKER) else {
            return Ok(None);
        };
        let destination = parse_bounded_path(destination, &mut self.path_bytes)?;
        self.cursor += 1;
        Ok(Some(destination))
    }

    fn parse_update_chunk(
        &mut self,
        operation: usize,
        path: &SafePath,
    ) -> Result<UpdateChunk, EngineFailure> {
        let context = match self.lines[self.cursor].as_str() {
            "@@" => None,
            marker if marker.starts_with("@@ ") => Some(marker[3..].to_owned()),
            marker if !marker.starts_with("@@") => {
                return Err(EngineFailure::new(
                    "invalid_update_hunk",
                    format!("update operation {operation} expected an '@@' hunk marker"),
                )
                .at(operation, path));
            }
            _ => {
                return Err(
                    EngineFailure::new("invalid_update_hunk", "invalid '@@' hunk marker")
                        .at(operation, path),
                );
            }
        };
        self.cursor += 1;

        let mut chunk = UpdateChunk {
            context,
            old_lines: Vec::new(),
            new_lines: Vec::new(),
            context_indices: Vec::new(),
            eof: false,
        };
        let mut changed = false;
        while self.body_remaining()
            && operation_header(&self.lines[self.cursor]).is_none()
            && !self.lines[self.cursor].starts_with("@@")
        {
            if self.lines[self.cursor] == END_OF_FILE_MARKER {
                chunk.eof = true;
                self.cursor += 1;
                break;
            }
            let line = &self.lines[self.cursor];
            let Some(prefix) = line.as_bytes().first().copied() else {
                return Err(EngineFailure::new(
                    "invalid_update_hunk",
                    "empty patch line must carry a ' ', '+', or '-' prefix",
                )
                .at(operation, path));
            };
            let text = line[1..].to_owned();
            match prefix {
                b' ' => {
                    chunk
                        .context_indices
                        .push((chunk.old_lines.len(), chunk.new_lines.len()));
                    chunk.old_lines.push(text.clone());
                    chunk.new_lines.push(text);
                }
                b'-' => {
                    changed = true;
                    chunk.old_lines.push(text);
                }
                b'+' => {
                    changed = true;
                    chunk.new_lines.push(text);
                }
                _ => {
                    return Err(EngineFailure::new(
                        "invalid_update_hunk",
                        "update lines must start with ' ', '+', or '-'",
                    )
                    .at(operation, path));
                }
            }
            self.cursor += 1;
        }
        if !changed {
            return Err(EngineFailure::new(
                "invalid_update_hunk",
                "update hunk contains no added or removed line",
            )
            .at(operation, path));
        }
        Ok(chunk)
    }

    fn body_remaining(&self) -> bool {
        self.cursor < self.lines.len() - 1
    }
}

enum OperationHeader<'a> {
    Add(&'a str),
    Delete(&'a str),
    Update(&'a str),
}

fn operation_header(line: &str) -> Option<OperationHeader<'_>> {
    if let Some(path) = line.strip_prefix(ADD_FILE_MARKER) {
        Some(OperationHeader::Add(path))
    } else if let Some(path) = line.strip_prefix(DELETE_FILE_MARKER) {
        Some(OperationHeader::Delete(path))
    } else {
        line.strip_prefix(UPDATE_FILE_MARKER)
            .map(OperationHeader::Update)
    }
}

fn parse_bounded_path(path: &str, path_bytes: &mut usize) -> Result<SafePath, EngineFailure> {
    *path_bytes = path_bytes
        .checked_add(path.len())
        .ok_or_else(|| EngineFailure::new("path_budget", "patch path budget overflow"))?;
    if *path_bytes > MAXIMUM_PATCH_PATH_BYTES_TOTAL {
        return Err(EngineFailure::new(
            "path_budget",
            format!("patch paths exceed {MAXIMUM_PATCH_PATH_BYTES_TOTAL} total bytes"),
        ));
    }
    SafePath::parse(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_operations_preserve_failure_context() {
        let cases = [
            ("bad", "invalid_patch", None, None),
            (
                "*** Add File: add.txt\nnot-added",
                "invalid_add_hunk",
                Some(0),
                Some("add.txt"),
            ),
            (
                "*** Update File: update.txt",
                "invalid_update_hunk",
                Some(0),
                Some("update.txt"),
            ),
            (
                "*** Update File: update.txt\n@@\n unchanged",
                "invalid_update_hunk",
                Some(0),
                Some("update.txt"),
            ),
        ];

        for (body, code, operation, path) in cases {
            let document = format!("{BEGIN_PATCH_MARKER}\n{body}\n{END_PATCH_MARKER}\n");
            let failure = parse_patch(&document).unwrap_err();
            assert_eq!(failure.code, code);
            assert_eq!(failure.operation, operation);
            assert_eq!(failure.path.as_deref(), path);
        }
    }
}
