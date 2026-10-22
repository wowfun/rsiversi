use rsi_process_native::{ProcessHandle, ProcessIdentity};
use std::{
    io,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer},
    sync::Mutex,
};
#[allow(unsafe_code)]
mod native;

/// Bounded local pipe name. Remote servers and arbitrary object namespaces are rejected.
#[derive(Clone, Debug)]
pub struct PipeName(String);
impl PipeName {
    /// Admits a local name before any operating-system operation.
    pub fn new(path: &Path) -> io::Result<Self> {
        let name = path
            .to_str()
            .ok_or_else(|| invalid("pipe name must be UTF-8"))?;
        let suffix = name
            .strip_prefix(r"\\.\pipe\rsi-")
            .ok_or_else(|| invalid("invalid local pipe namespace"))?;
        if suffix.is_empty()
            || suffix.len() > 160
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(invalid("invalid local pipe name"));
        }
        Ok(Self(name.into()))
    }
}

/// Exclusive local listener retaining an idle instance between accepts.
#[derive(Debug)]
pub struct PipeListener {
    name: PipeName,
    sid: String,
    next: Mutex<Pending>,
}
#[derive(Debug)]
struct Pending {
    instance: NamedPipeServer,
    stale: bool,
}
impl PipeListener {
    /// Claims a name with its first instance and a protected current-user DACL.
    pub fn bind(name: PipeName) -> io::Result<Self> {
        let sid = ProcessHandle::open(std::process::id())?
            .identity()
            .user_sid
            .clone();
        let next = Mutex::new(Pending {
            instance: native::create(&name.0, &sid, true)?,
            stale: false,
        });
        Ok(Self { name, sid, next })
    }

    /// Authenticates one client's actual process before returning a usable byte stream.
    pub async fn accept(&self) -> io::Result<LocalServerStream> {
        self.accept_with(|| native::create(&self.name.0, &self.sid, false))
            .await
    }

    async fn accept_with(
        &self,
        replacement: impl FnOnce() -> io::Result<NamedPipeServer>,
    ) -> io::Result<LocalServerStream> {
        let mut next = self.next.lock().await;
        if next.stale {
            let instance = native::create(&self.name.0, &self.sid, false)?;
            *next = Pending {
                instance,
                stale: false,
            };
        }
        next.instance.connect().await?;
        let replacement = match replacement() {
            Ok(replacement) => replacement,
            Err(error) => {
                next.stale = true;
                let _ = next.instance.disconnect();
                return Err(error);
            }
        };
        let connected = std::mem::replace(&mut next.instance, replacement);
        drop(next);
        let pid = native::client_pid(&connected)?;
        let process = ProcessHandle::open(pid)?;
        if process.identity().user_sid != self.sid || !process.is_running()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "foreign or exited pipe client",
            ));
        }
        Ok(LocalServerStream(connected))
    }
}

/// Opens and authenticates the expected server generation within a finite retry deadline.
pub async fn connect(name: &PipeName, expected: &ProcessIdentity) -> io::Result<LocalClientStream> {
    if ProcessHandle::open(std::process::id())?.identity().user_sid != expected.user_sid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "foreign expected pipe server",
        ));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let pipe = loop {
        match ClientOptions::new().open(&name.0) {
            Ok(pipe) => break pipe,
            Err(error)
                if error.raw_os_error() == Some(231) && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error),
        }
    };
    let observed = ProcessHandle::open(native::server_pid(&pipe)?)?;
    if observed.identity() != expected || !observed.is_running()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pipe server generation differs",
        ));
    }
    Ok(LocalClientStream(pipe))
}

/// Server byte stream admitted by actual client process identity.
#[derive(Debug)]
pub struct LocalServerStream(NamedPipeServer);
/// Client byte stream admitted by actual server process generation and user.
#[derive(Debug)]
pub struct LocalClientStream(NamedPipeClient);

macro_rules! stream {
    ($stream:ident) => {
        impl AsyncRead for $stream {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buffer: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Pin::new(&mut self.0).poll_read(cx, buffer)
            }
        }
        impl AsyncWrite for $stream {
            fn poll_write(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buffer: &[u8],
            ) -> Poll<io::Result<usize>> {
                Pin::new(&mut self.0).poll_write(cx, buffer)
            }
            fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Pin::new(&mut self.0).poll_flush(cx)
            }
            fn poll_shutdown(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<io::Result<()>> {
                Pin::new(&mut self.0).poll_shutdown(cx)
            }
        }
    };
}
stream!(LocalServerStream);
stream!(LocalClientStream);
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn replacement_failure_retains_name_and_allows_the_next_client() {
        let name = PipeName::new(Path::new(&format!(
            r"\\.\pipe\rsi-replacement-test-{}",
            std::process::id()
        )))
        .unwrap();
        let listener = PipeListener::bind(name.clone()).unwrap();
        let identity = ProcessHandle::open(std::process::id())
            .unwrap()
            .identity()
            .clone();
        let (client, failed) = tokio::join!(
            connect(&name, &identity),
            listener.accept_with(|| Err(io::Error::other("injected allocation failure")))
        );
        drop(client);
        assert!(
            failed
                .unwrap_err()
                .to_string()
                .contains("injected allocation")
        );
        assert!(PipeListener::bind(name.clone()).is_err());
        let (client, server) = tokio::join!(connect(&name, &identity), listener.accept());
        let mut client = client.unwrap();
        let mut server = server.unwrap();
        client.write_all(b"next").await.unwrap();
        let mut bytes = [0; 4];
        server.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"next");
    }

    #[tokio::test]
    async fn authenticated_pipe_rejects_squatting_and_wrong_generation() {
        let name = PipeName::new(Path::new(&format!(
            r"\\.\pipe\rsi-transport-test-{}",
            std::process::id()
        )))
        .unwrap();
        let listener = PipeListener::bind(name.clone()).unwrap();
        assert!(PipeListener::bind(name.clone()).is_err());
        let identity = ProcessHandle::open(std::process::id())
            .unwrap()
            .identity()
            .clone();
        let mut wrong = identity.clone();
        wrong.start_token.push('0');
        let (client, server) = tokio::join!(connect(&name, &wrong), listener.accept());
        assert!(client.is_err());
        drop(server.unwrap());
        let (client, server) = tokio::join!(connect(&name, &identity), listener.accept());
        let mut client = client.unwrap();
        let mut server = server.unwrap();
        client.write_all(b"verified").await.unwrap();
        let mut bytes = [0; 8];
        server.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"verified");
        assert!(PipeListener::bind(name).is_err());
    }

    #[test]
    fn pipe_names_cannot_target_remote_or_other_native_namespaces() {
        for name in [
            r"\\remote\pipe\rsi-test",
            r"\\.\pipe\other",
            r"\\.\pipe\rsi-a\b",
            r"\\.\pipe\rsi-",
        ] {
            assert!(PipeName::new(Path::new(name)).is_err());
        }
    }
}
