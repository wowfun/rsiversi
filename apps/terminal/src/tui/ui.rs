use super::editor::Editor;
use super::{Action, Client, Menu, Update, error, state::State};
use rsi_ui::{
    ActionInput, BoundView, PresentationLease, SnapshotPin, Ui, UiElement, UiReference, UiTarget,
};
use std::{collections::BTreeMap, sync::Arc};
use termina::event::{KeyCode, KeyEvent, Modifiers};

fn screenshot_preview(png: &[u8], columns: u16) -> super::Result<String> {
    use image::ImageDecoder as _;
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1280);
    limits.max_image_height = Some(720);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(error)?;
    if decoder.dimensions() != (1280, 720) {
        return Err(error("invalid viewport screenshot"));
    }
    let picture = image::DynamicImage::from_decoder(decoder).map_err(error)?;
    let width = u32::from(columns.clamp(1, 96));
    let height = (width * 27 / 96).max(1);
    let pixels = picture
        .resize_exact(width, height, image::imageops::FilterType::Triangle)
        .to_luma8();
    // Downsampling small dark text otherwise quantizes an entire light page to spaces.
    let (darkest, brightest) = pixels
        .pixels()
        .fold((u8::MAX, u8::MIN), |(dark, light), pixel| {
            (dark.min(pixel[0]), light.max(pixel[0]))
        });
    let mut text = String::new();
    for row in pixels.rows() {
        for pixel in row {
            let shade = if darkest == brightest {
                usize::from(pixel[0])
            } else {
                usize::from(pixel[0] - darkest) * 255 / usize::from(brightest - darkest)
            };
            text.push(['█', '▓', '▒', '░', ' '][shade * 5 / 256]);
        }
        text.push('\n');
    }
    Ok(text)
}

pub(super) struct Bindings {
    pub registry: Arc<Ui>,
    pub application: Arc<UiTarget>,
    pub surface: Arc<UiTarget>,
}
pub(super) struct Form {
    view: BoundView,
    fields: BTreeMap<String, String>,
    busy: bool,
    image: Option<(String, usize)>,
    pub(super) image_preview: Option<String>,
    pub(super) presentation: Option<(Arc<PresentationLease>, SnapshotPin)>,
}
pub(super) struct Edit {
    pub name: String,
    pub label: String,
    pub editor: Editor,
    multiline: bool,
}
impl Form {
    fn new(view: BoundView) -> Self {
        let fields = view
            .view
            .elements
            .iter()
            .filter_map(|element| {
                if let UiElement::Input { name, value, .. } = element {
                    Some((name.clone(), value.clone()))
                } else {
                    None
                }
            })
            .collect();
        Self {
            view,
            fields,
            busy: false,
            image: None,
            image_preview: None,
            presentation: None,
        }
    }
    fn from_snapshot(lease: Arc<PresentationLease>, pin: SnapshotPin) -> rsi_ui::Result<Self> {
        let model = pin.model().model;
        let image = model
            .data
            .get("image")
            .filter(|image| image["width"] == 1280 && image["height"] == 720)
            .and_then(|image| {
                Some((
                    image["source"].as_str()?.to_owned(),
                    usize::try_from(image["bytes"].as_u64()?).ok()?,
                ))
            })
            .filter(|(name, bytes)| {
                *bytes > 0
                    && *bytes <= 4 * 1024 * 1024
                    && model
                        .sources
                        .iter()
                        .any(|source| source.name == *name && source.media_type == "image/png")
            });
        let view = model.standard_view.ok_or_else(|| {
            rsi_ui::UiError::Invalid("Terminal requires a standard surface view".into())
        })?;
        let reference = lease.identity().reference.clone();
        let actions = model
            .actions
            .into_iter()
            .map(|action| {
                let mut bound = reference.clone();
                bound.name.clone_from(&action.name);
                (action.name, bound)
            })
            .collect();
        let mut form = Self::new(BoundView {
            reference,
            view,
            actions,
        });
        form.presentation = Some((lease, pin));
        form.image = image;
        Ok(form)
    }
    fn text(&self) -> String {
        use std::fmt::Write as _;
        let mut text = self.view.view.title.clone();
        if let Some(image) = &self.image_preview {
            let _ = write!(
                text,
                "\n\nCurrent screenshot · grayscale terminal preview\n{image}"
            );
        }

        for element in &self.view.view.elements {
            match element {
                UiElement::Text { text: value } | UiElement::Code { text: value } => {
                    let _ = write!(text, "\n\n{value}");
                }
                UiElement::Field { label, value } => {
                    let _ = write!(text, "\n\n{label}: {value}");
                }
                UiElement::Input { name, label, .. } => {
                    let _ = write!(text, "\n\n{label}: {}", self.fields[name]);
                }
                UiElement::Button { .. } => {}
            }
        }
        super::super::terminal_text(&text)
    }
    fn menu(&self, revision: u64) -> Menu {
        let mut items: Vec<_> = self
            .view
            .view
            .elements
            .iter()
            .filter_map(|element| match element {
                UiElement::Input { name, label, .. } => Some((
                    format!("Edit {}", super::super::terminal_text(label)),
                    Action::UiEdit(name.clone(), revision),
                )),
                UiElement::Button {
                    action,
                    label,
                    value,
                } => Some((
                    super::super::terminal_text(label),
                    Action::UiInvoke(self.view.actions[action].clone(), value.clone(), revision),
                )),
                _ => None,
            })
            .collect();
        if self.image.is_some() {
            items.push(("View current screenshot".into(), Action::UiImage(revision)));
        }
        Menu {
            title: super::super::terminal_text(&self.view.view.title),
            selected: 0,
            items,
        }
    }
}
impl State {
    pub(super) fn close_ui(&mut self) {
        self.ui_edit = None;
        if self.ui_form.take().is_some() {
            self.detail = None;
            self.detail_actions = None;
            self.detail_next = None;
            self.detail_previous = None;
        }
    }
    pub(super) fn refresh_ui(&mut self) {
        if let Some(form) = &self.ui_form {
            self.detail = Some(form.text());
            self.detail_offset = 0;
            self.detail_actions = (!form.busy).then(|| form.menu(self.view_revision));
        }
    }
    pub(super) fn ui_key(&mut self, key: KeyEvent) -> bool {
        let Some(edit) = &mut self.ui_edit else {
            return false;
        };
        if key.code == KeyCode::Enter && !key.modifiers.contains(Modifiers::SHIFT) {
            let mut edit = self.ui_edit.take().expect("editing UI field");
            if let Some(form) = &mut self.ui_form {
                form.fields.insert(edit.name, edit.editor.take());
            }
            self.refresh_ui();
        } else if !edit.multiline
            && (key.code == KeyCode::Enter
                || (key.code == KeyCode::Char('j') && key.modifiers.contains(Modifiers::CONTROL)))
        {
            self.notice("This field accepts one line");
        } else if let Err(message) = edit.editor.key(key) {
            self.notice(message);
        }
        true
    }
    pub(super) fn ui_paste(&mut self, text: &str) -> bool {
        let Some(edit) = &mut self.ui_edit else {
            return false;
        };
        if !edit.multiline && text.contains(['\r', '\n']) {
            self.notice("This field accepts one line");
        } else if let Err(message) = edit.editor.insert(text) {
            self.notice(message);
        }
        true
    }
}
impl Client {
    pub(super) fn action_menu(&mut self) {
        if self.state.ui_form.is_none() {
            self.state.invalidate_detail();
        }
        let mut menu = Menu::actions();
        if self
            .state
            .todos
            .as_ref()
            .is_none_or(|list| list.items().is_empty())
        {
            menu.items
                .retain(|(_, action)| !matches!(action, Action::Todos));
        }
        if self.state.header.fork_origin().is_none() {
            menu.items
                .retain(|(_, action)| !matches!(action, Action::Parent));
        }

        if self.ui.registry.has_block_renderers(&self.ui.surface) {
            menu.items.push(("Card details".into(), Action::UiCard));
        }
        for target in [&self.ui.application, &self.ui.surface] {
            if let Ok(surfaces) = self.ui.registry.surfaces(target) {
                menu.items.extend(surfaces.into_iter().map(|surface| {
                    (
                        super::super::terminal_text(&surface.title),
                        Action::UiSurface(surface.reference),
                    )
                }));
            }
        }
        self.state.menu = Some(menu);
    }
    pub(super) fn show_ui(&mut self, view: BoundView) {
        if !self.ui.registry.is_current(&view.reference) {
            return;
        }
        let form = Form::new(view);
        self.state.open_detail(form.text());
        self.state.ui_form = Some(form);
        self.state.refresh_ui();
        self.state
            .notice("Card details · Enter actions · Ctrl+Y copies displayed text");
    }
    pub(super) fn ui_surface(&mut self, reference: &UiReference) {
        self.state.close_ui();
        self.state.invalidate_detail();
        self.extension_view = None;
        let result = if self.ui.registry.matches_target(&self.ui.surface, reference)
            || self
                .ui
                .registry
                .matches_target(&self.ui.application, reference)
        {
            self.ui.registry.surface(reference)
        } else {
            Err(rsi_ui::UiError::Retired)
        };
        match result {
            Ok(view) => self.show_ui(view),
            // Snapshot-only surfaces require an asynchronous presentation lease.
            Err(rsi_ui::UiError::Invalid(_)) => match self.ui.registry.present(reference) {
                Ok(lease) => {
                    let lease = Arc::new(lease);
                    self.state.open_detail("Loading UI…".into());
                    self.spawn_detail(async move {
                        let pin = lease.ready().await.map_err(error)?;
                        Ok(Update::UiPresentation(lease, pin))
                    });
                }
                Err(problem) => self.state.notice(problem.to_string()),
            },
            Err(problem) => self.state.notice(problem.to_string()),
        }
    }
    pub(super) fn show_ui_presentation(&mut self, lease: Arc<PresentationLease>, pin: SnapshotPin) {
        match Form::from_snapshot(lease, pin) {
            Ok(mut form) => {
                let same = self.state.ui_form.as_ref().is_some_and(|previous| {
                    previous
                        .presentation
                        .as_ref()
                        .zip(form.presentation.as_ref())
                        .is_some_and(|((old, _), (new, _))| Arc::ptr_eq(old, new))
                        && !previous.busy
                });
                let edit = if same {
                    let previous = self.state.ui_form.as_ref().expect("same presentation");
                    for element in &previous.view.view.elements {
                        if let UiElement::Input { name, value, .. } = element
                            && let Some(edited) = previous.fields.get(name)
                            && edited != value
                            && form.fields.contains_key(name)
                        {
                            form.fields.insert(name.clone(), edited.clone());
                        }
                    }
                    self.state
                        .ui_edit
                        .take()
                        .filter(|edit| form.fields.contains_key(&edit.name))
                } else {
                    None
                };
                self.state.invalidate_detail();
                self.state.menu = None;
                self.state.open_detail(form.text());
                self.state.ui_form = Some(form);
                self.state.ui_edit = edit;
                self.state.refresh_ui();
                self.state.clear_info();
                self.watch_ui_presentation();
            }
            Err(problem) => self.state.notice(problem.to_string()),
        }
    }
    fn watch_ui_presentation(&mut self) {
        let Some((lease, pin)) = self
            .state
            .ui_form
            .as_ref()
            .and_then(|form| form.presentation.clone())
        else {
            return;
        };
        let revision = pin.revision();
        self.spawn_detail(async move {
            let mut changes = lease.changes();
            loop {
                let status = changes.borrow_and_update().clone();
                if status.stopped {
                    return Ok(Update::UiRetired);
                }
                if status.revision > revision {
                    let pin = match lease.snapshot() {
                        Err(rsi_ui::UiError::Retired) => return Ok(Update::UiRetired),
                        other => other
                            .map_err(error)?
                            .ok_or_else(|| error("UI snapshot unavailable"))?,
                    };
                    return Ok(Update::UiPresentation(lease, pin));
                }
                changes.changed().await.map_err(error)?;
            }
        });
    }
    pub(super) fn ui_card(&mut self) {
        let Some(block) = self.state.transcript.blocks.get(self.state.focused) else {
            self.state.info("No focused card");
            return;
        };
        let full = block.text();
        let window = rsi_conversation::FieldWindow::text(&full, 0, rsi_ui::MAXIMUM_VIEW_BYTES)
            .expect("UI block bound");
        let result = self.ui.registry.block(
            &self.ui.surface,
            &rsi_ui::BlockInput {
                key: &block.key,
                text: &window.text,
                tool: block.tool.as_ref(),
                sources: block.sources(),
            },
        );
        match result {
            Ok(Some(view)) => self.show_ui(view),
            Ok(None) => self.state.info("No active contribution renders this card"),
            Err(problem) => self.state.notice(problem.to_string()),
        }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "One fenced dispatcher handles displayed UI actions and revision-bound source reads."
    )]
    pub(super) fn ui_action(&mut self, action: Action) {
        let Some(form) = &mut self.state.ui_form else {
            return;
        };
        if form.busy || !self.ui.registry.is_current(&form.view.reference) {
            return;
        }
        match action {
            Action::UiImage(revision) if revision == self.state.view_revision => {
                let Some((name, bytes)) = form.image.clone() else {
                    return;
                };
                let Some((lease, pin)) = form.presentation.clone() else {
                    return;
                };
                form.busy = true;
                self.state.invalidate_detail();
                self.state.refresh_ui();
                let columns = super::terminal::size().0.min(96).saturating_sub(6);
                self.spawn_detail(async move {
                    let mut png = Vec::with_capacity(bytes);
                    while png.len() < bytes {
                        let chunk = lease
                            .source(
                                pin.revision(),
                                &name,
                                png.len() as u64,
                                (bytes - png.len()).min(65536),
                            )
                            .await
                            .map_err(error)?;
                        if chunk.is_empty() || chunk.len() > bytes - png.len() {
                            return Err(error("incomplete screenshot"));
                        }
                        png.extend_from_slice(chunk.as_ref());
                    }
                    tokio::task::spawn_blocking(move || screenshot_preview(&png, columns))
                        .await
                        .map_err(error)?
                        .map(Update::UiImage)
                });
                self.state.info("Reading screenshot…");
            }
            Action::UiEdit(name, revision) if revision == self.state.view_revision => {
                let Some((label, multiline)) = form.view.view.elements.iter().find_map(|element| {
                    if let UiElement::Input {
                        name: key,
                        label,
                        multiline,
                        ..
                    } = element
                        && *key == name
                    {
                        Some((label.clone(), *multiline))
                    } else {
                        None
                    }
                }) else {
                    return;
                };
                let value = form.fields[&name].clone();
                let other_bytes: usize = form
                    .fields
                    .iter()
                    .filter(|(key, _)| **key != name)
                    .map(|(_, value)| value.len())
                    .sum();
                let editor = Editor::with_text(
                    value,
                    rsi_ui::MAXIMUM_INPUT_BYTES.saturating_sub(other_bytes),
                );
                self.state.invalidate_detail();
                self.state.ui_edit = Some(Edit {
                    name,
                    label: super::super::terminal_text(&label),
                    editor,
                    multiline,
                });
                self.state.refresh_ui();
                self.watch_ui_presentation();
            }
            Action::UiInvoke(reference, value, revision)
                if revision == self.state.view_revision =>
            {
                if form.view.actions.get(&reference.name) != Some(&reference) {
                    return;
                }
                let input = ActionInput {
                    value,
                    fields: form.fields.clone(),
                };
                form.busy = true;
                let presentation = form.presentation.clone();
                self.state.invalidate_detail();
                self.state.refresh_ui();
                let stop = self.state.detail_stop.clone();
                if let Some((lease, pin)) = presentation {
                    let Some(action) = pin.action(&reference.name) else {
                        self.ui_failed();
                        return;
                    };
                    let work = lease.invoke(&action, input);
                    self.spawn_detail(async move {
                        let pin = work.await.map_err(error)?;
                        Ok(Update::UiPresentation(lease, pin))
                    });
                } else {
                    let work = self.ui.registry.invoke_in_view(&reference, input, stop);
                    self.spawn_detail(async move { work.await.map(Update::Ui).map_err(error) });
                }
                self.state.info("Working…");
            }
            _ => {}
        }
    }
    pub(super) fn ui_retired(&mut self) {
        self.state.invalidate_detail();
        self.state.close_ui();
        self.state.menu = None;
        self.state.clear_info();
    }
    pub(super) fn ui_failed(&mut self) {
        self.state.invalidate_detail();
        if let Some(form) = &mut self.state.ui_form {
            form.busy = false;
        }
        self.state.refresh_ui();
        self.watch_ui_presentation();
    }
    pub(super) fn show_ui_image(&mut self, text: String) {
        if let Some(form) = &mut self.state.ui_form {
            form.busy = false;
            form.image_preview = Some(text);
            self.state.refresh_ui();
            self.state.clear_info();
            self.watch_ui_presentation();
        }
    }
    pub(super) fn ui_changed(&mut self) {
        if self
            .state
            .ui_form
            .as_ref()
            .is_some_and(|form| !self.ui.registry.is_current(&form.view.reference))
        {
            self.state.invalidate_detail();
            self.state.close_ui();
            self.state.menu = None;
            self.state.notice("This UI contribution has retired");
        } else if self
            .state
            .menu
            .as_ref()
            .is_some_and(|menu| menu.title == "Actions")
        {
            self.action_menu();
        }
    }
}

#[cfg(test)]
mod preview_tests {
    use super::screenshot_preview;
    #[test]
    fn viewport_preview_is_finite_and_rejects_other_dimensions() {
        for (width, height, valid) in [
            (1280, 720, true),
            (1281, 1, false),
            (1, 721, false),
            (1, 1, false),
        ] {
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(width, height)
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            let result = screenshot_preview(png.get_ref(), 96);
            if valid {
                let text = result.unwrap();
                assert_eq!(text.lines().count(), 27);
                assert!(text.lines().all(|row| row.chars().count() == 96));
            } else {
                assert!(result.is_err());
            }
        }
    }
    #[test]
    fn preview_scales_to_the_available_terminal_columns() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1280, 720)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        for (columns, width, height) in [(0, 1, 1), (36, 36, 10), (96, 96, 27), (160, 96, 27)] {
            let preview = screenshot_preview(png.get_ref(), columns).unwrap();
            assert_eq!(preview.lines().count(), height);
            assert!(preview.lines().all(|row| row.chars().count() == width));
        }
    }
}
