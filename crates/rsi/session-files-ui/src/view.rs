use crate::{
    DIRECTORY_PAGE_ENTRIES, FILE_PAGE_BYTES, FilesBrowserContract,
    browser::{Operation, Request},
};
use rsi_files_protocol::{DirectoryPage, FileKind, FilePage, OpenedFile, RelativePath};
use rsi_meta::Context;
use rsi_ui::{Result, SurfaceRenderer, UiElement, UiError, UiView};

#[derive(Debug)]
pub(crate) struct Card;
impl SurfaceRenderer for Card {
    fn render(&self, target: &Context) -> Result<UiView> {
        let browser = target
            .lookup_local::<FilesBrowserContract>()
            .ok_or(UiError::Retired)?;
        let state = browser.state.lock().expect("Files browser state poisoned");
        Ok(initial(state.revision, state.opened.is_some()))
    }
}
fn button(revision: u64, label: impl Into<String>, operation: Operation) -> UiElement {
    UiElement::Button {
        action: "browse".into(),
        label: label.into(),
        value: serde_json::to_value(Request {
            revision: revision.to_string(),
            operation,
        })
        .expect("bounded Files payload"),
    }
}
pub(crate) fn initial(revision: u64, opened: bool) -> UiView {
    let mut elements = vec![
        UiElement::Input {
            name: "path".into(),
            label: "Workspace-relative path".into(),
            value: String::new(),
            multiline: false,
        },
        button(
            revision,
            "List directory",
            Operation::Input {
                kind: FileKind::Directory,
            },
        ),
        button(
            revision,
            "Read file",
            Operation::Input {
                kind: FileKind::File,
            },
        ),
        button(
            revision,
            "Workspace root",
            Operation::Open {
                path: RelativePath::default(),
                kind: FileKind::Directory,
            },
        ),
    ];
    if opened {
        elements.push(button(
            revision,
            "Current snapshot",
            Operation::Page {
                offset: 0,
                hex: false,
            },
        ));
        elements.push(button(revision, "Refresh", Operation::Refresh));
        elements.push(button(revision, "Release snapshot", Operation::Release));
    }
    UiView {
        title: "Workspace files".into(),
        elements,
    }
}
pub(crate) fn failure(revision: u64, opened: bool, error: &str) -> UiView {
    let mut view = initial(revision, opened);
    view.elements
        .insert(0, UiElement::Text { text: error.into() });
    view
}
fn preview(bytes: &[u8], maximum: usize) -> String {
    let mut text = rsi_tools_protocol::safe_tool_text(bytes);
    if text.len() > maximum {
        let mut end = maximum;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}
fn base(revision: u64, file: &OpenedFile) -> UiView {
    let mut view = initial(revision, true);
    view.elements.insert(
        0,
        UiElement::Field {
            label: "Path bytes (hex)".into(),
            value: serde_json::to_value(&file.path)
                .expect("validated path")
                .as_str()
                .expect("hex path")
                .into(),
        },
    );
    view.elements.insert(
        0,
        UiElement::Field {
            label: "Path".into(),
            value: preview(file.path.as_bytes(), 1024),
        },
    );
    view
}
pub(crate) fn file(revision: u64, file: &OpenedFile, page: &FilePage, hex: bool) -> UiView {
    let mut view = base(revision, file);
    view.title = "Workspace file".into();
    let bytes = decode(&page.bytes_hex);
    let end = page.offset + bytes.len() as u64;
    view.elements.push(UiElement::Field {
        label: "Bytes".into(),
        value: format!("{}–{} of {}", page.offset, end, page.total),
    });
    view.elements.push(UiElement::Code {
        text: if hex {
            rsi_conversation::hex_window(&bytes, page.offset).expect("bounded Files page")
        } else {
            rsi_tools_protocol::safe_tool_text(&bytes)
        },
    });
    view.elements.push(button(
        revision,
        if hex { "View text" } else { "View exact hex" },
        Operation::Page {
            offset: page.offset,
            hex: !hex,
        },
    ));
    if page.offset > 0 {
        view.elements.push(button(
            revision,
            "Previous page",
            Operation::Page {
                offset: page.offset.saturating_sub(FILE_PAGE_BYTES as u64),
                hex,
            },
        ));
    }
    if end < page.total {
        view.elements.push(button(
            revision,
            "Next page",
            Operation::Page { offset: end, hex },
        ));
    }
    view
}
pub(crate) fn directory(revision: u64, file: &OpenedFile, page: DirectoryPage) -> UiView {
    let mut view = base(revision, file);
    view.title = "Workspace directory".into();
    let end = page.offset + page.entries.len();
    view.elements.push(UiElement::Field {
        label: "Entries".into(),
        value: format!("{}–{} of {}", page.offset, end, page.total),
    });
    for entry in page.entries {
        let name = preview(entry.name.as_bytes(), 256);
        if let Some(kind) = entry.kind {
            view.elements.push(button(
                revision,
                format!(
                    "{name}{}",
                    if kind == FileKind::Directory { "/" } else { "" }
                ),
                Operation::Open {
                    path: entry.path,
                    kind,
                },
            ));
        } else {
            view.elements.push(UiElement::Field {
                label: name,
                value: "Link or special file · not readable".into(),
            });
        }
    }
    if page.offset > 0 {
        view.elements.push(button(
            revision,
            "Previous page",
            Operation::Page {
                offset: page.offset.saturating_sub(DIRECTORY_PAGE_ENTRIES) as u64,
                hex: false,
            },
        ));
    }
    if end < page.total {
        view.elements.push(button(
            revision,
            "Next page",
            Operation::Page {
                offset: end as u64,
                hex: false,
            },
        ));
    }
    view
}
fn decode(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_path_and_raw_byte_pages_fit_the_shared_view_and_action_limits() {
        let mut bytes = vec![1; rsi_files_protocol::MAXIMUM_FILE_PATH_BYTES];
        for index in (255..bytes.len()).step_by(256) {
            bytes[index] = b'/';
        }
        *bytes.last_mut().unwrap() = b'x';
        let opened = OpenedFile {
            path: RelativePath::new(&bytes).unwrap(),
            token: "0".repeat(32).try_into().unwrap(),
            kind: FileKind::File,
            length: FILE_PAGE_BYTES as u64 * 2,
        };
        let page = FilePage {
            offset: 0,
            total: opened.length,
            bytes_hex: "ff".repeat(FILE_PAGE_BYTES),
        };
        for hex in [false, true] {
            let view = file(u64::MAX, &opened, &page, hex);
            assert!(serde_json::to_vec(&view).unwrap().len() <= rsi_ui::MAXIMUM_VIEW_BYTES);
            for element in view.elements {
                if let UiElement::Button { value, .. } = element {
                    let input = rsi_ui::ActionInput {
                        value,
                        fields: std::collections::BTreeMap::from([(
                            "path".into(),
                            "\u{1}".repeat(crate::INPUT_PATH_BYTES),
                        )]),
                    };
                    assert!(
                        serde_json::to_vec(&input).unwrap().len() <= rsi_ui::MAXIMUM_INPUT_BYTES
                    );
                }
            }
        }
        assert_eq!(decode("00ff1b"), [0, 255, 27]);
        let value = serde_json::to_value(Operation::Page {
            offset: u64::MAX,
            hex: true,
        })
        .unwrap();
        assert_eq!(value["offset"], u64::MAX.to_string());
        assert!(matches!(
            serde_json::from_value::<Operation>(value).unwrap(),
            Operation::Page {
                offset: u64::MAX,
                hex: true
            }
        ));
        assert_eq!(
            rsi_conversation::hex_window(&[0, 255, 27], 4096).unwrap(),
            "00001000  00 ff 1b \n"
        );
    }
}
