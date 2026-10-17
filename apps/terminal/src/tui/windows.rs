//! Windows Terminal console ownership. No worker shares a console read or write.
#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
use super::input::{Framer, Input};
use std::{
    fs::{File, OpenOptions},
    io,
    os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use windows_sys::Win32::{
    Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::{
        Console::{
            ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT,
            ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_QUICK_EDIT_MODE,
            ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
            GetConsoleMode, INPUT_RECORD, KEY_EVENT, KEY_EVENT_RECORD, ReadConsoleInputW,
            SetConsoleMode, WINDOW_BUFFER_SIZE_EVENT, WriteConsoleW,
        },
        IO::CancelSynchronousIo,
        Threading::{CreateEventW, SetEvent, WaitForMultipleObjects},
    },
};

struct Modes {
    input: File,
    output: File,
    input_mode: u32,
    output_mode: u32,
}
static MODES: Mutex<Option<Modes>> = Mutex::new(None);
static RESTORATION: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static RESTORATION_FAILED: AtomicBool = AtomicBool::new(false);
struct ActiveOutput {
    stop: CancellationToken,
    ended: CancellationToken,
}
static OUTPUT: Mutex<Option<ActiveOutput>> = Mutex::new(None);
fn console(read: bool) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(if read { "CONIN$" } else { "CONOUT$" })
}
fn mode(file: &File) -> io::Result<u32> {
    let mut value = 0;
    // SAFETY: the owned console handle and writable DWORD remain live for this call.
    if unsafe { GetConsoleMode(file.as_raw_handle(), &raw mut value) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}
fn set_mode(file: &File, value: u32) -> io::Result<()> {
    // SAFETY: file retains a valid console handle; flags are documented console modes.
    if unsafe { SetConsoleMode(file.as_raw_handle(), value) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
pub(super) fn enter() -> io::Result<()> {
    enter_with(Duration::from_secs(2), acquire_modes)
}
fn enter_with(
    timeout: Duration,
    acquire: impl FnOnce() -> io::Result<Modes> + Send + 'static,
) -> io::Result<()> {
    let mut modes = MODES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if RESTORATION_FAILED.load(Ordering::Acquire) {
        return Err(io::Error::other(
            "terminal restoration could not start; restart the terminal tab",
        ));
    }
    if modes.is_some() {
        return Err(io::Error::other("terminal already acquired"));
    }
    {
        let mut previous = RESTORATION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if previous
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return Err(io::Error::other(
                "previous terminal restoration is waiting for the console host",
            ));
        }
        if let Some(worker) = previous.take() {
            let _ = worker.join();
        }
    }
    let (sender, receive) = std::sync::mpsc::sync_channel(0);
    let worker = std::thread::Builder::new()
        .name("rsi-console-acquire".into())
        .spawn(move || {
            if let Err(std::sync::mpsc::SendError(Ok(modes))) = sender.send(acquire()) {
                // The caller timed out. This worker still owns the acquired modes
                // and restores them before admitting another terminal owner.
                let _ = set_mode(&modes.input, modes.input_mode);
                let _ = set_mode(&modes.output, modes.output_mode);
            }
        })?;
    let result = receive.recv_timeout(timeout);
    drop(receive);
    *RESTORATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
    *modes = Some(result.map_err(|error| {
        io::Error::new(
            if matches!(error, std::sync::mpsc::RecvTimeoutError::Timeout) {
                io::ErrorKind::TimedOut
            } else {
                io::ErrorKind::BrokenPipe
            },
            "console mode acquisition did not finish; its owner is retained for cleanup",
        )
    })??);
    Ok(())
}
fn acquire_modes() -> io::Result<Modes> {
    let input = console(true)?;
    let output = console(false)?;
    let input_mode = mode(&input)?;
    let output_mode = mode(&output)?;
    let raw =
        (input_mode | ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_WINDOW_INPUT | ENABLE_EXTENDED_FLAGS)
            & !(ENABLE_ECHO_INPUT
                | ENABLE_LINE_INPUT
                | ENABLE_PROCESSED_INPUT
                | ENABLE_QUICK_EDIT_MODE
                | ENABLE_MOUSE_INPUT);
    set_mode(&input, raw)?;
    if let Err(error) = set_mode(
        &output,
        output_mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    ) {
        let _ = set_mode(&input, input_mode);
        return Err(error);
    }
    Ok(Modes {
        input,
        output,
        input_mode,
        output_mode,
    })
}
pub(super) fn restore(bytes: &[u8]) {
    // Keep acquisition fenced until the restoration owner has been installed.
    let mut modes = MODES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let output = OUTPUT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(output) = &output {
        output.stop.cancel();
    }
    if let Some(modes) = modes.take() {
        let data = bytes.to_vec();
        // Even SetConsoleMode may wait for a stalled ConPTY host. This one retained
        // worker keeps all handles alive without holding application shutdown open.
        if let Ok(worker) = std::thread::Builder::new()
            .name("rsi-console-restore".into())
            .spawn(move || {
                if let Some(output) = output {
                    // The cancellation worker is the sole owner of cancellation
                    // and joining. Never overlap restoration with an unsettled write.
                    while !output.ended.is_cancelled() {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
                let _ = write_console(&modes.output, &data, None);
                let _ = set_mode(&modes.input, modes.input_mode);
                let _ = set_mode(&modes.output, modes.output_mode);
            })
        {
            let deadline = std::time::Instant::now() + Duration::from_millis(200);
            while !worker.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            // Retain without foreground cancellation: CancelSynchronousIo can
            // itself block on a stalled console host. Re-entry stays fenced.
            *RESTORATION
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
        } else {
            RESTORATION_FAILED.store(true, Ordering::Release);
        }
    }
}

pub(super) fn input(
    stop: CancellationToken,
    tasks: &TaskTracker,
) -> io::Result<mpsc::Receiver<Input>> {
    let file = console(true)?;
    // SAFETY: null attributes/name request an unnamed, non-inheritable manual-reset event.
    let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful CreateEventW transfers its sole owning handle here.
    let event = Arc::new(unsafe { OwnedHandle::from_raw_handle(raw) });
    let signal = event.clone();
    let cancelled = stop.child_token();
    let wake = cancelled.clone();
    tasks.spawn(async move {
        wake.cancelled().await;
        // SAFETY: Arc pins the event through this call; SetEvent doesn't consume it.
        unsafe {
            SetEvent(signal.as_raw_handle());
        }
    });
    let (sender, receiver) = mpsc::channel(8);
    let token = tasks.token();
    let runtime = tokio::runtime::Handle::current();
    std::thread::Builder::new()
        .name("rsi-console-input".into())
        .spawn(move || {
            let _token = token;
            let _cancel = cancelled.drop_guard();
            read_console(&file, &event, &stop, |input| {
                runtime.block_on(async {
                    tokio::select! {
                        biased;
                        () = stop.cancelled() => false,
                        result = sender.send(input) => result.is_ok(),
                    }
                })
            });
        })?;
    Ok(receiver)
}

fn read_console(
    file: &File,
    event: &OwnedHandle,
    stop: &CancellationToken,
    send: impl Fn(Input) -> bool,
) {
    let handles = [event.as_raw_handle(), file.as_raw_handle()];
    let mut framer = Framer::default();
    let mut high = None;
    loop {
        if stop.is_cancelled() {
            return;
        }
        // SAFETY: both owned handles and the array live throughout the bounded wait.
        match unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, 50) } {
            WAIT_OBJECT_0 => return,
            WAIT_TIMEOUT => {
                if let Some(input) = framer.flush_escape()
                    && !send(input)
                {
                    return;
                }
                continue;
            }
            value if value == WAIT_OBJECT_0 + 1 => (),
            _ => break,
        }
        let mut records = [INPUT_RECORD::default(); 128];
        let mut count = 0;
        // SAFETY: this is the sole console reader; the writable array has 128 records.
        if unsafe {
            ReadConsoleInputW(
                file.as_raw_handle(),
                records.as_mut_ptr(),
                128,
                &raw mut count,
            )
        } == 0
        {
            break;
        }
        for record in &records[..count as usize] {
            if u32::from(record.EventType) == WINDOW_BUFFER_SIZE_EVENT {
                let (columns, rows) = super::terminal::size();
                if !send(Input::Terminal(termina::Event::WindowResized(
                    termina::WindowSize {
                        cols: columns,
                        rows,
                        pixel_width: None,
                        pixel_height: None,
                    },
                ))) {
                    return;
                }
            } else if u32::from(record.EventType) == KEY_EVENT {
                // SAFETY: EventType selects the initialized KEY_EVENT_RECORD union member.
                let key = unsafe { record.Event.KeyEvent };
                if !key_input(&mut framer, &mut high, key, &send) {
                    return;
                }
            }
        }
    }
    let _ = send(Input::Closed);
}

fn key_input(
    framer: &mut Framer,
    high: &mut Option<u16>,
    key: KEY_EVENT_RECORD,
    send: &impl Fn(Input) -> bool,
) -> bool {
    // VK_MENU releases carry committed Alt composition text. ConPTY also uses
    // these releases for surrogate pairs that have no keyboard-layout mapping.
    const VK_MENU: u16 = 0x12;
    if key.bKeyDown == 0 && key.wVirtualKeyCode != VK_MENU {
        return true;
    }
    // SAFETY: the caller supplies the KEY_EVENT_RECORD returned by ReadConsoleInputW.
    let unit = unsafe { key.uChar.UnicodeChar };
    if unit == 0 {
        return true;
    }
    for _ in 0..key.wRepeatCount {
        let character = if (0xd800..=0xdbff).contains(&unit) {
            *high = Some(unit);
            continue;
        } else if (0xdc00..=0xdfff).contains(&unit) {
            high.take().and_then(|lead| {
                char::from_u32(
                    0x10000 + ((u32::from(lead) - 0xd800) << 10) + u32::from(unit) - 0xdc00,
                )
            })
        } else {
            *high = None;
            char::from_u32(u32::from(unit))
        };
        let Some(character) = character else {
            if !send(Input::Rejected("Invalid terminal UTF-16")) {
                return false;
            }
            continue;
        };
        let mut bytes = [0; 4];
        for byte in character.encode_utf8(&mut bytes).as_bytes() {
            if let Some(input) = framer.advance(*byte)
                && !send(input)
            {
                return false;
            }
        }
    }
    true
}

type WriteRequest = (Vec<u8>, oneshot::Sender<io::Result<()>>);

fn write_console(file: &File, bytes: &[u8], stop: Option<&CancellationToken>) -> io::Result<()> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut characters = text.chars().peekable();
    let mut wide = [0_u16; 2048];
    loop {
        let mut length = 0;
        while let Some(&character) = characters.peek() {
            let units = character.len_utf16();
            if length + units > wide.len() {
                break;
            }
            character.encode_utf16(&mut wide[length..length + units]);
            length += units;
            let _ = characters.next();
        }
        if length == 0 {
            return Ok(());
        }
        let mut offset = 0;
        while offset < length {
            if stop.is_some_and(CancellationToken::is_cancelled) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "console writer retired",
                ));
            }
            let remaining = u32::try_from(length - offset).expect("bounded console buffer");
            let mut written = 0;
            // SAFETY: file owns CONOUT$; the initialized UTF-16 slice and result
            // DWORD stay live through this synchronous call. Reserved is null.
            if unsafe {
                WriteConsoleW(
                    file.as_raw_handle(),
                    wide[offset..].as_ptr().cast(),
                    remaining,
                    &raw mut written,
                    std::ptr::null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if written == 0 || written > remaining {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "console write made invalid progress",
                ));
            }
            offset += written as usize;
        }
    }
}

pub(super) struct Output {
    sender: Option<mpsc::Sender<WriteRequest>>,
    stop: CancellationToken,
}
impl Output {
    pub(super) fn new(stop: CancellationToken) -> io::Result<Self> {
        Self::start(stop, write_console)
    }
    fn start(
        stop: CancellationToken,
        write: impl Fn(&File, &[u8], Option<&CancellationToken>) -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        Self::start_with(stop, write, |task| {
            std::thread::Builder::new()
                .name("rsi-console-cancel".into())
                .spawn(task)
                .map(drop)
        })
    }
    fn start_with(
        stop: CancellationToken,
        write: impl Fn(&File, &[u8], Option<&CancellationToken>) -> io::Result<()> + Send + 'static,
        spawn_cancel: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
    ) -> io::Result<Self> {
        // Use the same lock order as restoration. No native writer can appear
        // after the acquired console has been handed to its restoration owner.
        let modes = MODES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut active = OUTPUT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if modes.is_none() || stop.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "terminal owner retired",
            ));
        }
        if active.is_some() {
            return Err(io::Error::other("terminal already has an output owner"));
        }
        let file = console(false)?;
        let (sender, mut receive) = mpsc::channel::<WriteRequest>(1);
        let ended = CancellationToken::new();
        let ended_worker = ended.clone().drop_guard();
        let runtime = tokio::runtime::Handle::current();
        let writer_runtime = runtime.clone();
        let stopping = stop.clone();
        let worker = std::thread::Builder::new()
            .name("rsi-console-output".into())
            .spawn(move || {
                let _ended = ended_worker;
                while let Some((bytes, reply)) = writer_runtime.block_on(async {
                    tokio::select! { biased;
                        () = stopping.cancelled() => None,
                        request = receive.recv() => request,
                    }
                }) {
                    if stopping.is_cancelled() {
                        break;
                    }
                    let result = write(&file, &bytes, Some(&stopping));
                    let failed = result.is_err();
                    let _ = reply.send(result);
                    if failed {
                        break;
                    }
                }
            })?;
        let handle = worker.as_handle().try_clone_to_owned()?;
        let output = ActiveOutput {
            stop: stop.clone(),
            ended: ended.clone(),
        };
        let cancel = stop.clone();
        spawn_cancel(Box::new(move || {
            runtime.block_on(async {
                tokio::select! {
                    () = cancel.cancelled() => (),
                    () = ended.cancelled() => (),
                }
            });
            while !ended.is_cancelled() {
                if cancel.is_cancelled() {
                    // SAFETY: this owned thread handle refers only to our writer, never a reused pool thread.
                    unsafe {
                        CancelSynchronousIo(handle.as_raw_handle());
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            let _ = worker.join();
        }))?;
        *active = Some(output);
        Ok(Self {
            sender: Some(sender),
            stop,
        })
    }
    pub(super) async fn write(&self, bytes: Vec<u8>) -> io::Result<()> {
        let (send, receive) = oneshot::channel();
        self.sender
            .as_ref()
            .expect("writer open")
            .try_send((bytes, send))
            .map_err(io::Error::other)?;
        tokio::select! { biased;
            () = self.stop.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "console writer retired")),
            result = receive => result.map_err(io::Error::other)?,
        }
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        self.stop.cancel();
        self.sender.take();
    }
}
