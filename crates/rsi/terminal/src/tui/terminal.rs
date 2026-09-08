//! One coalescing writer receives complete frames. Input never reads through Crossterm.
use crossterm::{cursor, event, terminal};
use ratatui::{
    backend::{Backend as _, CrosstermBackend},
    buffer::Buffer,
};
#[cfg(unix)]
use std::io::Write as _;
use std::io::{self, IsTerminal as _};
use std::sync::atomic::{AtomicBool, Ordering};
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

pub(super) struct Terminal {
    pub(super) frames: watch::Sender<Option<Vec<u8>>>,
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
        let task = tasks.spawn(writer(receiver, command_receiver, setup, stop.clone()));
        Ok(Self {
            frames,
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
    mut frames: watch::Receiver<Option<Vec<u8>>>,
    mut commands: mpsc::Receiver<String>,
    setup: Vec<u8>,
    stop: CancellationToken,
) -> io::Result<()> {
    let tty = tokio::io::unix::AsyncFd::new(tty(false)?)?;
    write_bytes(&tty, &setup, &stop).await?;
    loop {
        let frame = tokio::select! {
            () = stop.cancelled() => return Ok(()),
            Some(command) = commands.recv() => Some(command.into_bytes()),
            changed = frames.changed() => {
                if changed.is_err() { return Ok(()); }
                frames.borrow_and_update().clone()
            },
        };
        if let Some(frame) = frame {
            write_bytes(&tty, &frame, &stop).await?;
        }
    }
}

#[cfg(unix)]
async fn write_bytes(
    tty: &tokio::io::unix::AsyncFd<std::fs::File>,
    bytes: &[u8],
    stop: &CancellationToken,
) -> io::Result<()> {
    let mut remaining = bytes;
    while !remaining.is_empty() {
        if !ACTIVE.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut ready = tokio::select! {
            () = stop.cancelled() => return Ok(()),
            ready = tty.writable() => ready?,
        };
        match ready.try_io(|fd| fd.get_ref().write(remaining)) {
            Ok(Ok(0)) => return Err(io::Error::new(io::ErrorKind::WriteZero, "terminal closed")),
            Ok(Ok(count)) => remaining = &remaining[count..],
            Ok(Err(error)) => return Err(error),
            Err(_) => {}
        }
    }
    Ok(())
}

#[cfg(not(unix))]
async fn writer(
    _: watch::Receiver<Option<Vec<u8>>>,
    _: mpsc::Receiver<String>,
    _: Vec<u8>,
    _: CancellationToken,
) -> io::Result<()> {
    Err(io::Error::other("Unix terminal required"))
}

pub(super) fn frame(buffer: &Buffer) -> io::Result<Vec<u8>> {
    use unicode_width::UnicodeWidthStr as _;
    let mut bytes = Vec::new();
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
                    u16::try_from(index % width).unwrap_or(0),
                    u16::try_from(index / width).unwrap_or(0),
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

    #[tokio::test]
    async fn terminal_guard_child() {
        let Ok(mode) = std::env::var("RSI_TUI_GUARD_TEST_CHILD") else {
            return;
        };
        let terminal = Terminal::enter(&tokio_util::task::TaskTracker::new()).unwrap();
        if mode == "slow" {
            for _ in 0..100 {
                terminal.frames.send_replace(Some(vec![b'x'; 1024 * 1024]));
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
        use std::io::Read as _;
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
            let reader = {
                let mut reader = pair.master.try_clone_reader().unwrap();
                Some(std::thread::spawn(move || {
                    let mut output = Vec::new();
                    let _ = reader.read_to_end(&mut output);
                    output
                }))
            };
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
            if let Some(reader) = reader {
                let bytes = reader.join().unwrap();
                if mode == "panic" {
                    assert!(bytes.windows(RESTORE.len()).any(|bytes| bytes == RESTORE));
                }
            }
        }
    }
}
