use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

pub(super) struct Delivery {
    pub(super) status: String,
    pub(super) osc: Option<String>,
}

pub(super) async fn copy(text: String) -> Delivery {
    for (program, args, reader, read_args) in [
        (
            "wl-copy",
            vec!["--type", "text/plain;charset=utf-8"],
            "wl-paste",
            vec!["--no-newline", "--type", "text/plain;charset=utf-8"],
        ),
        (
            "xclip",
            vec!["-selection", "clipboard", "-in"],
            "xclip",
            vec!["-selection", "clipboard", "-out"],
        ),
    ] {
        let result = native(
            &text,
            program,
            &args,
            reader,
            &read_args,
            Duration::from_millis(800),
        )
        .await;
        if result.is_ok() {
            return Delivery {
                status: format!("Copied and verified via {program}"),
                osc: None,
            };
        }
    }
    osc52(&text)
}

fn osc52(text: &str) -> Delivery {
    if text.len().div_ceil(3) * 4 > 32 * 1024 {
        return Delivery { status: "Copy failed: native clipboard unavailable; selection exceeds OSC52's 32 KiB encoded limit".into(), osc: None };
    }
    Delivery {
        status: "OSC52 copy requested; terminal delivery is unverified".into(),
        osc: Some(format!("\x1b]52;c;{}\x07", STANDARD.encode(text))),
    }
}

async fn native(
    text: &str,
    program: &str,
    args: &[&str],
    reader: &str,
    read_args: &[&str],
    deadline: Duration,
) -> std::io::Result<()> {
    tokio::time::timeout(deadline, async {
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("clipboard stdin unavailable"))?;
        stdin.write_all(text.as_bytes()).await?;
        stdin.shutdown().await?;
        drop(stdin);
        if !child.wait().await?.success() {
            return Err(std::io::Error::other("clipboard helper failed"));
        }
        let mut child = tokio::process::Command::new(reader)
            .args(read_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("clipboard stdout unavailable"))?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if child.wait().await?.success() && bytes == text.as_bytes() {
            Ok(())
        } else {
            Err(std::io::Error::other("clipboard readback differs"))
        }
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "clipboard helper deadline"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_is_bounded_and_never_claims_confirmed_delivery() {
        let text = "x".repeat(24 * 1024);
        let delivery = osc52(&text);
        assert!(delivery.status.contains("unverified"));
        let osc = delivery.osc.unwrap();
        let encoded = osc
            .strip_prefix("\x1b]52;c;")
            .unwrap()
            .strip_suffix('\x07')
            .unwrap();
        assert_eq!(encoded.len(), 32 * 1024);
        assert_eq!(STANDARD.decode(encoded).unwrap(), text.as_bytes());
        assert!(osc52(&(text + "x")).osc.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_delivery_requires_exact_readback_and_bounds_helper_time() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let writer = directory.path().join("writer");
        let reader = directory.path().join("reader");
        std::fs::write(&writer, "#!/bin/sh\ncat >/dev/null\n").unwrap();
        std::fs::write(&reader, "#!/bin/sh\nprintf verified\n").unwrap();
        for path in [&writer, &reader] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let deadline = Duration::from_millis(500);
        assert!(
            native(
                "verified",
                writer.to_str().unwrap(),
                &[],
                reader.to_str().unwrap(),
                &[],
                deadline
            )
            .await
            .is_ok()
        );
        assert!(
            native(
                "different",
                writer.to_str().unwrap(),
                &[],
                reader.to_str().unwrap(),
                &[],
                deadline
            )
            .await
            .is_err()
        );
        std::fs::write(&writer, "#!/bin/sh\nexec sleep 30\n").unwrap();
        let start = std::time::Instant::now();
        let result = native(
            "verified",
            writer.to_str().unwrap(),
            &[],
            reader.to_str().unwrap(),
            &[],
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
