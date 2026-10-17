use super::*;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::Write;

const INPUT_TIMEOUT: Duration = Duration::from_secs(5);
const INPUT_STOP_TIMEOUT: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const SETTLEMENT_DELAY: Duration = Duration::from_millis(1500);
const RESTORE_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn cancelled_frame_releases_waiter_before_native_reply() {
    let (sender, mut receive) = mpsc::channel(1);
    let stop = CancellationToken::new();
    let output = Output {
        sender: Some(sender),
        stop: stop.clone(),
    };
    let mut writing = Box::pin(output.write(b"pending frame".to_vec()));
    assert!(futures_util::poll!(writing.as_mut()).is_pending());
    let (bytes, reply) = receive.try_recv().unwrap();
    stop.cancel();
    let error = tokio::time::timeout(Duration::from_secs(1), writing)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    // The native owner still owns the bytes and reply after its waiter is gone.
    assert_eq!(bytes, b"pending frame");
    assert!(reply.send(Ok(())).is_err());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One child owns the entire console acquisition and restoration trace.
async fn console_child() {
    let Some(directory) = std::env::var_os("RSI_CONSOLE_PROBE") else {
        return;
    };
    let root = std::path::PathBuf::from(directory);
    let blocked = std::env::var_os("RSI_CONSOLE_BLOCKED").is_some();
    let cancel_frame = std::env::var_os("RSI_CONSOLE_CANCEL_FRAME").is_some();
    if !blocked {
        // SAFETY: this child owns an isolated ConPTY; 437 is a valid legacy code page.
        assert_ne!(
            unsafe { windows_sys::Win32::System::Console::SetConsoleOutputCP(437) },
            0
        );
    }
    let before = (
        mode(&console(true).unwrap()).unwrap(),
        mode(&console(false).unwrap()).unwrap(),
    );
    let (release, held) = std::sync::mpsc::sync_channel(0);
    let (ready, acquired) = std::sync::mpsc::sync_channel(1);
    let started = std::time::Instant::now();
    let error = enter_with(Duration::from_millis(20), move || {
        let modes = acquire_modes()?;
        ready.send(()).unwrap();
        held.recv().unwrap();
        Ok(modes)
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(1));
    acquired.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        enter().is_err(),
        "pending acquisition must retain the owner fence"
    );
    release.send(()).unwrap();
    tokio::time::timeout(RESTORE_TIMEOUT, async {
        while !RESTORATION.lock().unwrap().as_ref().unwrap().is_finished() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        before,
        (
            mode(&console(true).unwrap()).unwrap(),
            mode(&console(false).unwrap()).unwrap()
        )
    );
    enter().unwrap();
    let startup = Output::start_with(CancellationToken::new(), write_console, |_task| {
        Err(io::Error::other(
            "injected cancellation worker spawn failure",
        ))
    });
    assert!(startup.err().unwrap().to_string().contains("injected"));
    assert!(
        OUTPUT.lock().unwrap().is_none(),
        "failed startup must not publish an owner"
    );
    // The current-thread runtime cannot run the SetEvent task during this wait.
    let stalled_tasks = TaskTracker::new();
    let stalled_stop = CancellationToken::new();
    let mut stalled_input = input(stalled_stop.clone(), &stalled_tasks).unwrap();
    stalled_stop.cancel();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        match stalled_input.try_recv() {
            Err(mpsc::error::TryRecvError::Disconnected) => break,
            _ => assert!(
                std::time::Instant::now() < deadline,
                "native input depends on runtime scheduling"
            ),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    stalled_tasks.close();
    tokio::time::timeout(INPUT_STOP_TIMEOUT, stalled_tasks.wait())
        .await
        .unwrap();
    // Cancellation must wake an idle output owner even while its sender is held.
    let idle_stop = CancellationToken::new();
    let idle_output = Output::new(idle_stop.clone()).unwrap();
    let idle_ended = OUTPUT.lock().unwrap().as_ref().unwrap().ended.clone();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!idle_ended.is_cancelled());
    idle_stop.cancel();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !idle_ended.is_cancelled() {
        assert!(
            std::time::Instant::now() < deadline,
            "output settlement depends on runtime scheduling"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(idle_output);
    restore(b"");
    tokio::time::timeout(RESTORE_TIMEOUT, async {
        while !RESTORATION.lock().unwrap().as_ref().unwrap().is_finished() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    enter().unwrap();
    let tasks = TaskTracker::new();
    let stop = CancellationToken::new();
    let mut input = input(stop.clone(), &tasks).unwrap();
    std::fs::write(root.join("ready"), b"ready").unwrap();
    let mut pasted = false;
    let mut resized = false;
    let mut key_received = false;
    let mut mouse_received = false;
    tokio::time::timeout(INPUT_TIMEOUT, async {
        while !(pasted && resized && key_received && mouse_received) {
            match input.recv().await.unwrap() {
                Input::Terminal(termina::Event::Paste(text)) => {
                    assert_eq!(text, "hello 中文🦀\nsecond line");
                    pasted = true;
                }
                Input::Terminal(termina::Event::WindowResized(size))
                    if size.cols == 101 && size.rows == 31 =>
                {
                    resized = true;
                }
                Input::Closed => panic!("console input closed"),
                Input::Terminal(termina::Event::Key(key))
                    if key.code == termina::event::KeyCode::Up =>
                {
                    key_received = true;
                }
                Input::Terminal(termina::Event::Mouse(mouse))
                    if mouse.column == 3 && mouse.row == 2 =>
                {
                    mouse_received = true;
                }
                _ => (),
            }
        }
    })
    .await
    .unwrap();
    stop.cancel();
    drop(input);
    tasks.close();
    tokio::time::timeout(INPUT_STOP_TIMEOUT, tasks.wait())
        .await
        .unwrap();
    std::fs::write(root.join("stage"), b"input closed; starting blocked output").unwrap();
    let stop = CancellationToken::new();
    let output = if cancel_frame {
        Output::start(stop.clone(), |file, bytes, stop| {
            write_console(file, b"\x1b[?1049h\x1b[?25lpartial frame", None)?;
            stop.unwrap().cancel();
            write_console(file, bytes, stop)
        })
    } else {
        Output::new(stop.clone())
    }
    .unwrap();
    assert!(
        Output::new(CancellationToken::new()).is_err(),
        "second writer must be refused"
    );
    if blocked {
        let cancel = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });
    }
    let started = std::time::Instant::now();
    if cancel_frame {
        assert!(output.write(vec![b'x'; 8192]).await.is_err());
    } else if blocked {
        let _ = output.write(vec![b'x'; 16 * 1024 * 1024]).await;
    } else {
        output
            .write(
                format!(
                    "\x1b[?2004h\x1b[?25l{}中文🦀 console restore probe\r\n",
                    "x".repeat(4095)
                )
                .into_bytes(),
            )
            .await
            .unwrap();
    }
    std::fs::write(root.join("stage"), b"output returned; retiring worker").unwrap();
    drop(output);
    assert!(started.elapsed() < WRITE_TIMEOUT);
    // Simulate delayed writer settlement independently of whether this Windows
    // host happens to cancel WriteConsoleW promptly. The fallback release bounds
    // a broken implementation too, so the parent can still reap the child.
    if !blocked {
        let ended = CancellationToken::new();
        OUTPUT.lock().unwrap().as_mut().unwrap().ended = ended.clone();
        std::thread::spawn(move || {
            std::thread::sleep(SETTLEMENT_DELAY);
            ended.cancel();
        });
    }
    std::fs::write(root.join("stage"), b"output retired; restoring modes").unwrap();
    let restoring = std::time::Instant::now();
    restore(b"\x1b[?2004l\x1b[?25h\x1b[?1049l");
    std::fs::write(
        root.join("stage"),
        format!("restore returned after {:?}", restoring.elapsed()),
    )
    .unwrap();
    assert!(restoring.elapsed() < Duration::from_secs(1));
    let restoration = RESTORATION.lock().unwrap().as_ref().unwrap().thread().id();
    restore(b"must not replace an unsettled restoration owner");
    assert_eq!(
        RESTORATION.lock().unwrap().as_ref().unwrap().thread().id(),
        restoration
    );
    assert!(
        enter().is_err(),
        "another terminal must not race pending writer or mode restoration"
    );
    if !blocked {
        assert_ne!(
            mode(&console(true).unwrap()).unwrap(),
            before.0,
            "modes must remain acquired until the writer has ended"
        );
    }
    std::fs::write(root.join("restore-returned"), b"bounded").unwrap();
    tokio::time::timeout(RESTORE_TIMEOUT, async {
        loop {
            if RESTORATION
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let after = (
        mode(&console(true).unwrap()).unwrap(),
        mode(&console(false).unwrap()).unwrap(),
    );
    assert_eq!(before, after);
    assert!(
        Output::new(CancellationToken::new()).is_err(),
        "retired owner must refuse a delayed writer"
    );
    // Do not yield after entering: the queued async writer has not run when Drop
    // restores. It must not create a new native writer when it is later polled.
    let tasks = TaskTracker::new();
    let terminal = super::super::terminal::Terminal::enter(&tasks).unwrap();
    drop(terminal);
    tasks.close();
    tokio::time::timeout(Duration::from_secs(2), tasks.wait())
        .await
        .unwrap();
    assert!(OUTPUT.lock().unwrap().is_none());
    if !blocked {
        // SAFETY: queries the isolated child's current console, without pointers.
        assert_eq!(
            unsafe { windows_sys::Win32::System::Console::GetConsoleOutputCP() },
            437
        );
    }
    std::fs::write(
        root.join("passed"),
        b"paste, resize, input cancellation, blocked output cancellation, exact mode restoration",
    )
    .unwrap();
}

#[test]
fn conpty_paste_resize_cancel_and_restore() {
    probe(true, false);
}
#[test]
fn conpty_normal_output_and_mode_restore() {
    probe(false, false);
}
#[test]
fn conpty_cancelled_frame_restores_screen_and_cursor() {
    probe(false, true);
}
#[allow(clippy::too_many_lines)] // One ConPTY owner coordinates capture, input and cancellation.
fn probe(blocked: bool, cancel_frame: bool) {
    let root = tempfile::tempdir().unwrap();
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
        "tui::windows::tests::console_child",
        "--nocapture",
    ]);
    command.env("RSI_CONSOLE_PROBE", root.path());
    if blocked {
        command.env("RSI_CONSOLE_BLOCKED", "1");
    }
    if cancel_frame {
        command.env("RSI_CONSOLE_CANCEL_FRAME", "1");
    }
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
    let reply = writer.clone();
    let draining = Arc::new(AtomicBool::new(true));
    let drain = draining.clone();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let capture = std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut bytes = [0; 8192];
        let mut cursor_replied = false;
        loop {
            while !drain.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(10));
            }
            match std::io::Read::read(&mut reader, &mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if output.len() < 8192 {
                        output.extend_from_slice(&bytes[..count]);
                    }
                    if !cursor_replied && output.windows(4).any(|bytes| bytes == b"\x1b[6n") {
                        reply.lock().unwrap().write_all(b"\x1b[1;1R").unwrap();
                        cursor_replied = true;
                    }
                }
            }
        }
        output
    });
    // Includes startup and each sequential child budget, plus reporting slack.
    let deadline = std::time::Instant::now()
        + Duration::from_secs(6)
        // The timed-out acquisition probe also waits for acquisition and rollback.
        + Duration::from_secs(2)
        + RESTORE_TIMEOUT
        + INPUT_TIMEOUT
        + INPUT_STOP_TIMEOUT
        + WRITE_TIMEOUT
        + SETTLEMENT_DELAY
        + RESTORE_TIMEOUT;
    while !root.path().join("ready").exists() {
        if std::time::Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            drop(pair.master);
            panic!(
                "console child did not become ready: {}",
                String::from_utf8_lossy(&capture.join().unwrap())
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    draining.store(!blocked, Ordering::Release);
    pair.master
        .resize(PtySize {
            rows: 31,
            cols: 101,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    writer
        .lock()
        .unwrap()
        .write_all("\x1b[200~hello 中文🦀\nsecond line\x1b[201~\x1b[A\x1b[<0;4;3M".as_bytes())
        .unwrap();
    while !root.path().join("passed").exists() && std::time::Instant::now() < deadline {
        if root.path().join("restore-returned").exists() {
            draining.store(true, Ordering::Release);
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let passed = root.path().join("passed").exists();
    draining.store(true, Ordering::Release);
    if !passed {
        let _ = child.kill();
    }
    let status = child.wait().unwrap();
    drop(pair.master);
    let output = capture.join().unwrap();
    assert!(
        passed && status.success(),
        "stage={}; {}",
        std::fs::read_to_string(root.path().join("stage")).unwrap_or_default(),
        String::from_utf8_lossy(&output[output.len().saturating_sub(2000)..])
    );
    if cancel_frame {
        assert!(
            output
                .windows(b"\x1b[?25h\x1b[?1049l".len())
                .any(|bytes| bytes == b"\x1b[?25h\x1b[?1049l"),
            "cancelled frame did not restore the cursor and screen: {}",
            String::from_utf8_lossy(&output)
        );
    } else if !blocked {
        assert!(
            String::from_utf8_lossy(&output).contains("中文🦀"),
            "Unicode output was lost: {}",
            String::from_utf8_lossy(&output)
        );
    }
}
