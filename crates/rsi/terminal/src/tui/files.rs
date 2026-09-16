use super::*;
use rsi_files_protocol::{FileKind, RelativePath};
use rsi_session_files_ui::{FilePickerPage, FilePickerRequest};

impl Client {
    pub(super) fn file_picker(&mut self, request: FilePickerRequest) {
        let Some(browser) = self.files.clone() else {
            self.state
                .notice("Files are unavailable for this conversation");
            return;
        };
        if self.state.file_insert.is_none() {
            let cursor = self.state.editor.cursor();
            let start = if self.state.editor.text()[..cursor].ends_with('@') {
                cursor - 1
            } else {
                cursor
            };
            self.state.file_insert = Some(start..cursor);
        }
        self.state.open_detail("Reading workspace files…".into());
        let stop = self.state.detail_stop.clone();
        self.spawn_detail(async move {
            browser
                .pick(request, stop)
                .await
                .map_err(error)?
                .map(Update::FilePicker)
                .ok_or_else(|| error("File picker closed"))
        });
    }
    pub(super) fn insert_file(&mut self, locator: &str) {
        let Some(range) = self.state.file_insert.clone() else {
            self.state.notice("Reopen the file picker from this draft");
            return;
        };
        if range.end != self.state.editor.cursor() {
            self.state
                .notice("Draft cursor changed; reopen the file picker");
            return;
        }
        match self.state.editor.replace_range(range, locator) {
            Ok(()) => {
                self.state.escape();
                self.state
                    .info("File path inserted; file contents were not added");
            }
            Err(problem) => self.state.notice(problem),
        }
    }
}
impl State {
    #[expect(
        clippy::too_many_lines,
        reason = "One exhaustive projection keeps related state transitions and ownership visible together"
    )]
    pub(super) fn show_file_picker(&mut self, page: FilePickerPage) {
        let parent = page
            .path
            .as_bytes()
            .iter()
            .rposition(|byte| *byte == b'/')
            .map_or(&b""[..], |index| &page.path.as_bytes()[..index]);
        let open_parent = Action::FilePicker(FilePickerRequest::Open {
            path: RelativePath::new(parent).expect("validated parent path"),
            file_kind: FileKind::Directory,
        });
        let page_action = |offset: String, hex| {
            Action::FilePicker(FilePickerRequest::Page {
                revision: page.revision.clone(),
                offset,
                hex,
            })
        };
        let offset: u64 = page.offset.parse().expect("validated offset");
        if page.kind == FileKind::Directory {
            self.detail = None;
            self.detail_actions = None;
            let mut items = Vec::new();
            if !page.path.as_bytes().is_empty() {
                items.push(("../ Parent directory".into(), open_parent));
            }
            for entry in page.entries {
                if let Some(kind) = entry.kind {
                    items.push((
                        format!(
                            "{}{}",
                            entry.name,
                            if kind == FileKind::Directory {
                                "/"
                            } else {
                                " · preview"
                            }
                        ),
                        Action::FilePicker(FilePickerRequest::Open {
                            path: entry.path,
                            file_kind: kind,
                        }),
                    ));
                }
            }
            if offset > 0 {
                items.push((
                    "Previous directory page".into(),
                    page_action(offset.saturating_sub(16).to_string(), false),
                ));
            }
            if page.more {
                items.push((
                    "Next directory page".into(),
                    page_action(page.next_offset.clone(), false),
                ));
            }
            items.push((
                "Reference a conversation…".into(),
                Action::ReferenceSources(None),
            ));
            self.menu = Some(Menu {
                title: format!(
                    "@ Workspace files · entries {}–{} / {}",
                    page.offset, page.next_offset, page.total
                ),
                items,
                selected: 0,
            });
            self.info("Enter opens directory or previews file · Esc keeps draft");
        } else {
            let locator = page.locator.expect("regular file locator");
            self.open_detail(format!(
                "{}\nBytes {}–{} / {}\n\n{}",
                super::super::terminal_text(&locator),
                page.offset,
                page.next_offset,
                page.total,
                page.text
            ));
            self.detail_previous =
                (offset > 0).then(|| page_action(offset.saturating_sub(4096).to_string(), false));
            self.detail_next = page
                .more
                .then(|| page_action(page.next_offset.clone(), false));
            self.detail_actions = Some(Menu {
                title: "File reference".into(),
                selected: 0,
                items: vec![
                    ("Insert path into draft".into(), Action::InsertFile(locator)),
                    (
                        "View exact hex".into(),
                        page_action(page.offset.clone(), true),
                    ),
                    ("View text".into(), page_action(page.offset.clone(), false)),
                    ("Parent directory".into(), open_parent),
                    (
                        "Refresh current file".into(),
                        Action::FilePicker(FilePickerRequest::Open {
                            path: page.path.clone(),
                            file_kind: FileKind::File,
                        }),
                    ),
                ],
            });
            self.info("Enter actions · ←/→ pages · Esc keeps draft");
        }
    }
}
