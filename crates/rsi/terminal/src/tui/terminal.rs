//! One coalescing writer receives complete frames. Input never reads through Crossterm.
use crossterm::{cursor, event, terminal};
use ratatui::{
    backend::{Backend as _, CrosstermBackend},
    buffer::Buffer,
};
#[cfg(unix)]
use std::io::Write as _;
use std::io::{self, IsTerminal as _};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[cfg(unix)]
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

static ACTIVE: AtomicBool = AtomicBool::new(false);
const RESTORE: &[u8] =
    b"\x1b[<u\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1006l\x1b[0m\x1b[?25h\x1b[?1049l";

pub(super) fn check() -> Result<(), &'static str> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("tui requires terminal stdin and stdout");
    }
    if !cfg!(unix) {
        return Err("tui terminal input currently requires Unix");
    }
    Ok(())
}

pub(super) fn size() -> (u16, u16) {
    let (width, height) = terminal::size().unwrap_or((80, 24));
    (width.clamp(1, 512), height.clamp(1, 256))
}

#[derive(Debug)]
pub(super) struct RenderedFrame {
    pub(super) generation: u64,
    pub(super) revision: u64,
    pub(super) buffer: Buffer,
    pub(super) view: super::render::View,
}

pub(super) struct Terminal {
    pub(super) frames: watch::Sender<Option<Arc<RenderedFrame>>>,
    pub(super) presented: watch::Receiver<Option<Arc<RenderedFrame>>>,
    pub(super) commands: mpsc::Sender<String>,
    stop: CancellationToken,
    task: Option<tokio::task::JoinHandle<io::Result<()>>>,
}

impl Terminal {
    pub(super) fn enter(tasks: &tokio_util::task::TaskTracker) -> io::Result<Self> {
        static HOOK: std::sync::Once = std::sync::Once::new();
        HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                previous(info);
            }));
        });
        terminal::enable_raw_mode()?;
        ACTIVE.store(true, Ordering::Release);
        let (frames, receiver) = watch::channel(None);
        let (presented_sender, presented) = watch::channel(None);
        let stop = CancellationToken::new();
        let (commands, command_receiver) = mpsc::channel(2);
        let mut setup = Vec::new();
        crossterm::execute!(
            setup,
            terminal::EnterAlternateScreen,
            cursor::Hide,
            event::EnableBracketedPaste,
            event::EnableMouseCapture,
            event::PushKeyboardEnhancementFlags(
                event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            )
        )?;
        // The only reader recognizes protocol responses; legacy terminals ignore this query.
        setup.extend_from_slice(b"\x1b[?u");
        let task = tasks.spawn(writer(
            receiver,
            command_receiver,
            presented_sender,
            setup,
            stop.clone(),
        ));
        Ok(Self {
            frames,
            presented,
            commands,
            stop,
            task: Some(task),
        })
    }

    pub(super) fn failed(&self) -> bool {
        self.task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
    }

    pub(super) async fn close(mut self) -> io::Result<()> {
        self.stop.cancel();
        // Drop restores even when a writer has failed or a future is cancelled.
        let result = self
            .task
            .take()
            .expect("writer task owned until close")
            .await
            .map_err(io::Error::other)?;
        restore();
        result
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.stop.cancel();
        restore();
    }
}

fn restore() {
    if !ACTIVE.swap(false, Ordering::AcqRel) {
        return;
    }
    #[cfg(unix)]
    if let Ok(mut tty) = tty(false) {
        let start = std::time::Instant::now();
        let mut remaining = RESTORE;
        while !remaining.is_empty() && start.elapsed() < Duration::from_millis(200) {
            match tty.write(remaining) {
                Ok(0) => break,
                Ok(count) => remaining = &remaining[count..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => break,
            }
        }
    }
    let _ = terminal::disable_raw_mode();
}

#[cfg(unix)]
fn tty(read: bool) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(read)
        .write(!read)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open("/dev/tty")
}

#[cfg(unix)]
async fn writer(
    frames: watch::Receiver<Option<Arc<RenderedFrame>>>,
    commands: mpsc::Receiver<String>,
    presented: watch::Sender<Option<Arc<RenderedFrame>>>,
    setup: Vec<u8>,
    stop: CancellationToken,
) -> io::Result<()> {
    let tty = tokio::io::unix::AsyncFd::new(tty(false)?)?;
    if write_bytes(&tty, &setup, &stop, &ACTIVE).await? == WriteStatus::Interrupted {
        return Ok(());
    }
    write_frames(&tty, frames, commands, presented, &stop, &ACTIVE).await
}

#[cfg(unix)]
async fn write_frames(
    tty: &tokio::io::unix::AsyncFd<std::fs::File>,
    mut frames: watch::Receiver<Option<Arc<RenderedFrame>>>,
    mut commands: mpsc::Receiver<String>,
    presented: watch::Sender<Option<Arc<RenderedFrame>>>,
    stop: &CancellationToken,
    active: &AtomicBool,
) -> io::Result<()> {
    let mut last: Option<Arc<RenderedFrame>> = None;
    loop {
        tokio::select! {
            () = stop.cancelled() => return Ok(()),
            Some(command) = commands.recv() => {
                if write_bytes(tty, command.as_bytes(), stop, active).await? == WriteStatus::Interrupted {
                    return Ok(());
                }
            },
            changed = frames.changed() => {
                if changed.is_err() { return Ok(()); }
                let Some(next) = frames.borrow_and_update().clone() else { continue; };
                if last.as_ref().is_some_and(|last| next.revision <= last.revision) { continue; }
                let previous = last.as_ref().filter(|last| last.generation == next.generation);
                let bytes = frame(&next.buffer, previous.map(|last| &last.buffer))?;
                if write_bytes(tty, &bytes, stop, active).await? == WriteStatus::Interrupted {
                    return Ok(());
                }
                last = Some(next.clone());
                presented.send_replace(Some(next));
            },
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteStatus {
    Complete,
    Interrupted,
}

#[cfg(unix)]
async fn write_bytes(
    tty: &tokio::io::unix::AsyncFd<std::fs::File>,
    bytes: &[u8],
    stop: &CancellationToken,
    active: &AtomicBool,
) -> io::Result<WriteStatus> {
    if stop.is_cancelled() || !active.load(Ordering::Acquire) {
        return Ok(WriteStatus::Interrupted);
    }
    let mut remaining = bytes;
    while !remaining.is_empty() {
        if !active.load(Ordering::Acquire) {
            return Ok(WriteStatus::Interrupted);
        }
        let mut ready = tokio::select! {
            biased;
            () = stop.cancelled() => return Ok(WriteStatus::Interrupted),
            ready = tty.writable() => ready?,
        };
        match ready.try_io(|fd| fd.get_ref().write(remaining)) {
            Ok(Ok(0)) => return Err(io::Error::new(io::ErrorKind::WriteZero, "terminal closed")),
            Ok(Ok(count)) => remaining = &remaining[count..],
            Ok(Err(error)) => return Err(error),
            Err(_) => {}
        }
    }
    Ok(WriteStatus::Complete)
}

#[cfg(not(unix))]
async fn writer(
    _: watch::Receiver<Option<Arc<RenderedFrame>>>,
    _: mpsc::Receiver<String>,
    _: watch::Sender<Option<Arc<RenderedFrame>>>,
    _: Vec<u8>,
    _: CancellationToken,
) -> io::Result<()> {
    Err(io::Error::other("Unix terminal required"))
}

fn frame(buffer: &Buffer, previous: Option<&Buffer>) -> io::Result<Vec<u8>> {
    use unicode_width::UnicodeWidthStr as _;
    let mut bytes = Vec::new();
    if let Some(previous) = previous.filter(|previous| previous.area == buffer.area) {
        let mut updates = previous.diff_iter(buffer).peekable();
        if updates.peek().is_none() {
            return Ok(bytes);
        }
        let mut backend = CrosstermBackend::new(&mut bytes);
        backend.draw(updates)?;
        ratatui::backend::Backend::flush(&mut backend)?;
        return Ok(bytes);
    }
    bytes.extend_from_slice(b"\x1b[0m\x1b[2J");
    let mut backend = CrosstermBackend::new(&mut bytes);
    let width = usize::from(buffer.area.width);
    let mut skip_until = 0;
    backend.draw(
        buffer
            .content
            .iter()
            .enumerate()
            .filter_map(|(index, cell)| {
                if index < skip_until {
                    return None;
                }
                skip_until = index + cell.symbol().width().max(1);
                Some((
                    buffer.area.x + u16::try_from(index % width).unwrap_or(0),
                    buffer.area.y + u16::try_from(index / width).unwrap_or(0),
                    cell,
                ))
            }),
    )?;
    ratatui::backend::Backend::flush(&mut backend)?;
    Ok(bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    mod capture;

    #[test]
    fn identical_cells_emit_no_terminal_bytes() {
        let buffer = Buffer::with_lines(["hello 界"]);
        assert!(frame(&buffer, Some(&buffer)).unwrap().is_empty());
    }

    #[test]
    fn diff_clears_wide_cells_changes_style_and_repaints_resize() {
        use ratatui::{backend::TestBackend, style::Color};
        crossterm::style::force_color_output(true);
        let mut previous = Buffer::with_lines(["界ab"]);
        previous[(0, 0)].set_bg(Color::Blue);
        let mut next = Buffer::with_lines(["x ab"]);
        next[(2, 0)].set_fg(Color::Red);
        let mut terminal = TestBackend::new(4, 1);
        terminal
            .draw(Buffer::empty(previous.area).diff_iter(&previous))
            .unwrap();
        terminal.draw(previous.diff_iter(&next)).unwrap();
        terminal.assert_buffer(&next);
        let delta = frame(&next, Some(&previous)).unwrap();
        let mut parser = vt100::Parser::new(1, 4, 0);
        parser.process(&frame(&previous, None).unwrap());
        parser.process(&delta);
        assert_eq!(parser.screen().contents(), "x ab");
        assert_eq!(
            parser.screen().cell(0, 1).unwrap().bgcolor(),
            vt100::Color::Default
        );
        assert_eq!(
            parser.screen().cell(0, 2).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
        assert!(!delta.windows(4).any(|part| part == b"\x1b[2J"));
        assert!(
            previous
                .diff_iter(&next)
                .any(|(x, y, cell)| x == 1 && y == 0 && cell.symbol() == " ")
        );
        assert!(delta.contains(&b' '));
        let resized = Buffer::with_lines(["smaller"]);
        assert_eq!(
            frame(&resized, Some(&next)).unwrap(),
            frame(&resized, None).unwrap()
        );
    }

    fn rendered(generation: u64, revision: u64, buffer: Buffer) -> Arc<RenderedFrame> {
        Arc::new(RenderedFrame {
            generation,
            revision,
            buffer,
            view: super::super::render::View::default(),
        })
    }

    fn output_pair() -> (
        tokio::io::unix::AsyncFd<std::fs::File>,
        tokio::net::UnixStream,
    ) {
        let (output, input) = std::os::unix::net::UnixStream::pair().unwrap();
        output.set_nonblocking(true).unwrap();
        input.set_nonblocking(true).unwrap();
        let output = std::fs::File::from(std::os::fd::OwnedFd::from(output));
        (
            tokio::io::unix::AsyncFd::new(output).unwrap(),
            tokio::net::UnixStream::from_std(input).unwrap(),
        )
    }

    #[tokio::test]
    async fn coalesced_frames_diff_from_written_cells_and_generation_repaints() {
        use tokio::io::AsyncReadExt as _;
        let (output, mut input) = output_pair();
        let (frames, receiver) = watch::channel(None);
        let (_commands, commands) = mpsc::channel(2);
        let (presented, mut acknowledged) = watch::channel(None);
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let task = tokio::spawn(async move {
            write_frames(
                &output,
                receiver,
                commands,
                presented,
                &stopping,
                &AtomicBool::new(true),
            )
            .await
        });
        let a = rendered(1, 1, Buffer::with_lines(["AAAA"]));
        frames.send_replace(Some(a.clone()));
        acknowledged.changed().await.unwrap();
        let mut first = vec![0; frame(&a.buffer, None).unwrap().len()];
        input.read_exact(&mut first).await.unwrap();
        assert_eq!(first, frame(&a.buffer, None).unwrap());

        // No yield between these sends: B cannot become the writer's baseline.
        frames.send_replace(Some(rendered(1, 2, Buffer::with_lines(["BBBB"]))));
        let c = rendered(1, 3, Buffer::with_lines(["AACA"]));
        frames.send_replace(Some(c.clone()));
        acknowledged.changed().await.unwrap();
        assert_eq!(
            acknowledged.borrow_and_update().as_ref().unwrap().revision,
            3
        );
        let expected = frame(&c.buffer, Some(&a.buffer)).unwrap();
        let mut bytes = vec![0; expected.len()];
        input.read_exact(&mut bytes).await.unwrap();
        assert_eq!(bytes, expected);

        frames.send_replace(Some(rendered(1, 4, c.buffer.clone())));
        acknowledged.changed().await.unwrap();
        assert_eq!(
            input.try_read(&mut [0; 1]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        frames.send_replace(Some(rendered(2, 5, c.buffer.clone())));
        acknowledged.changed().await.unwrap();
        let expected = frame(&c.buffer, None).unwrap();
        let mut bytes = vec![0; expected.len()];
        input.read_exact(&mut bytes).await.unwrap();
        assert_eq!(bytes, expected);
        stop.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn interrupted_partial_frame_does_not_advance_presented_cells() {
        use tokio::io::AsyncReadExt as _;
        let (output, mut input) = output_pair();
        let (frames, receiver) = watch::channel(None);
        let (_commands, commands) = mpsc::channel(2);
        let (presented, mut acknowledged) = watch::channel(None);
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let task = tokio::spawn(async move {
            write_frames(
                &output,
                receiver,
                commands,
                presented,
                &stopping,
                &AtomicBool::new(true),
            )
            .await
        });
        let a = rendered(1, 1, Buffer::with_lines(["AAAA"]));
        frames.send_replace(Some(a.clone()));
        acknowledged.changed().await.unwrap();
        let mut first = vec![0; frame(&a.buffer, None).unwrap().len()];
        input.read_exact(&mut first).await.unwrap();

        // Alternate styles keep this valid maximum-size screen larger than the
        // socket buffer, so one byte proves progress while the rest stays blocked.
        let mut buffer = Buffer::filled(
            ratatui::layout::Rect::new(0, 0, 512, 256),
            ratatui::buffer::Cell::new("x"),
        );
        for (index, cell) in buffer.content.iter_mut().enumerate() {
            cell.set_fg(if index % 2 == 0 {
                ratatui::style::Color::Red
            } else {
                ratatui::style::Color::Blue
            });
        }
        frames.send_replace(Some(rendered(1, 2, buffer)));
        input.read_exact(&mut [0; 1]).await.unwrap();
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(acknowledged.borrow().as_ref().unwrap().revision, 1);
    }

    #[tokio::test]
    async fn terminal_guard_child() {
        let Ok(mode) = std::env::var("RSI_TUI_GUARD_TEST_CHILD") else {
            return;
        };
        let terminal = Terminal::enter(&tokio_util::task::TaskTracker::new()).unwrap();
        if mode == "slow" {
            for revision in 1..=100 {
                let buffer = Buffer::filled(
                    ratatui::layout::Rect::new(0, 0, 512, 256),
                    ratatui::buffer::Cell::new("x"),
                );
                terminal.frames.send_replace(Some(Arc::new(RenderedFrame {
                    generation: 1,
                    revision,
                    buffer,
                    view: super::super::render::View::default(),
                })));
                tokio::task::yield_now().await;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_ne!(mode, "panic", "intentional terminal restoration probe");
        tokio::time::timeout(Duration::from_secs(2), terminal.close())
            .await
            .unwrap()
            .unwrap();
        std::fs::write(std::env::var("RSI_TUI_GUARD_TEST_DONE").unwrap(), b"closed").unwrap();
    }

    #[test]
    fn panic_restores_terminal_and_blocked_writer_does_not_prevent_exit() {
        for mode in ["panic", "slow"] {
            let directory = tempfile::tempdir().unwrap();
            let completed = directory.path().join("closed");
            let pair = native_pty_system()
                .openpty(PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .unwrap();
            let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
            command.args([
                "--exact",
                "tui::terminal::tests::terminal_guard_child",
                "--nocapture",
            ]);
            command.env("RSI_TUI_GUARD_TEST_CHILD", mode);
            command.env("RSI_TUI_GUARD_TEST_DONE", &completed);
            command.env("TERM", "xterm-256color");
            let mut child = pair.slave.spawn_command(command).unwrap();
            drop(pair.slave);
            if mode == "slow" {
                let start = std::time::Instant::now();
                while !completed.exists() {
                    if start.elapsed() > Duration::from_secs(5) {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("blocked output prevented terminal close");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                assert!(
                    format!("{:?}", pair.master.get_termios().unwrap().local_flags)
                        .contains("ICANON")
                );
            }
            // Only resume draining after the blocked writer has restored termios;
            // libtest itself prints its result synchronously after this point.
            let reader = capture::PtyCapture::start(pair.master.as_ref());
            let start = std::time::Instant::now();
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if start.elapsed() > Duration::from_secs(5) {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("{mode}: terminal writer prevented exit");
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            assert_eq!(status.success(), mode != "panic");
            assert!(
                format!("{:?}", pair.master.get_termios().unwrap().local_flags).contains("ICANON")
            );
            let bytes = reader.finish();
            if mode == "panic" {
                assert!(bytes.windows(RESTORE.len()).any(|bytes| bytes == RESTORE));
            }
        }
    }
}
