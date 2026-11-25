use crate::settings::{ConnectionProfile, DatabaseKind, Transport};
use std::{
    io,
    net::IpAddr,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, UnixListener, UnixStream},
    process::Command,
    task::{JoinHandle, JoinSet},
};

pub(crate) struct Relay {
    directory: PathBuf,
    socket: PathBuf,
    worker: JoinHandle<()>,
    error: Arc<Mutex<Option<String>>>,
}

impl Relay {
    pub(crate) fn start(profile: &ConnectionProfile) -> Result<Option<Arc<Self>>, String> {
        if profile.transport.is_direct() {
            return Ok(None);
        }
        // Keep the path within sockaddr_un's limit and prevent access by other users.
        let directory = PathBuf::from("/tmp").join(format!("dbdeck-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|error| format!("Не удалось создать каталог туннеля: {error}"))?;
        let socket = match profile.kind {
            DatabaseKind::PostgreSql => directory.join(format!(".s.PGSQL.{}", profile.port)),
            _ => directory.join("db.sock"),
        };
        let listener = UnixListener::bind(&socket).map_err(|error| {
            let _ = std::fs::remove_dir(&directory);
            format!("Не удалось открыть сокет туннеля: {error}")
        })?;
        let route = Route {
            transport: profile.transport.clone(),
            host: profile.host.clone(),
            port: profile.port,
        };
        let error = Arc::new(Mutex::new(None));
        let worker = tokio::spawn(serve(listener, Arc::new(route), error.clone()));
        Ok(Some(Arc::new(Self {
            directory,
            socket,
            worker,
            error,
        })))
    }

    pub(crate) fn socket_path(&self, kind: DatabaseKind) -> &Path {
        match kind {
            DatabaseKind::PostgreSql => &self.directory,
            _ => &self.socket,
        }
    }

    pub(crate) fn take_diagnostic(&self) -> Option<String> {
        self.error.lock().unwrap().take()
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.worker.abort();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

struct Route {
    transport: Transport,
    host: String,
    port: u16,
}

async fn serve(listener: UnixListener, route: Arc<Route>, error: Arc<Mutex<Option<String>>>) {
    let mut connections = JoinSet::new();
    while let Ok((mut socket, _)) = listener.accept().await {
        while connections.try_join_next().is_some() {}
        let route = route.clone();
        let error = error.clone();
        connections.spawn(async move {
            if let Err(message) = route.forward(&mut socket).await {
                *error.lock().unwrap() = Some(message);
            }
        });
    }
}

impl Route {
    async fn forward(&self, local: &mut UnixStream) -> Result<(), String> {
        match &self.transport {
            Transport::Direct => unreachable!("direct profiles do not start a relay"),
            Transport::Ssh {
                host,
                port,
                username,
                identity_file,
                known_hosts,
            } => {
                let authority = authority(&self.host, self.port);
                let mut command = Command::new("ssh");
                command.args([
                    "-F",
                    "/dev/null",
                    "-T",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "StrictHostKeyChecking=yes",
                    "-o",
                    "ConnectTimeout=6",
                    "-o",
                    "ServerAliveInterval=15",
                    "-o",
                    "ServerAliveCountMax=2",
                    "-o",
                    "PreferredAuthentications=publickey",
                    "-p",
                    &port.to_string(),
                    "-l",
                    username,
                    "-W",
                    &authority,
                ]);
                if let Some(path) = identity_file {
                    command.arg("-i").arg(path);
                }
                if let Some(path) = known_hosts {
                    let mut option = std::ffi::OsString::from("UserKnownHostsFile=");
                    option.push(path);
                    command.arg("-o").arg(option);
                }
                let mut child = command
                    .arg("--")
                    .arg(host)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|error| format!("Не удалось запустить OpenSSH: {error}"))?;
                let stdout = child.stdout.take().expect("SSH stdout is piped");
                let stdin = child.stdin.take().expect("SSH stdin is piped");
                if forward_stream(local, stdout, stdin).await == Some(0) {
                    return Err("Не удалось открыть SSH-туннель. Проверьте ключ, known_hosts и разрешение TCP forwarding.".into());
                }
            }
            Transport::Socks5 { host, port } | Transport::HttpConnect { host, port } => {
                let connect = async {
                    let mut remote = TcpStream::connect((host.as_str(), *port)).await?;
                    if matches!(self.transport, Transport::Socks5 { .. }) {
                        socks5(&mut remote, &self.host, self.port).await?;
                    } else {
                        http_connect(&mut remote, &authority(&self.host, self.port)).await?;
                    }
                    Ok::<_, io::Error>(remote)
                };
                let mut remote = tokio::time::timeout(Duration::from_secs(6), connect)
                    .await
                    .map_err(|_| "Прокси не открыл туннель за 6 с".to_owned())?
                    .map_err(|error| format!("Прокси не открыл туннель: {error}"))?;
                let (input, output) = remote.split();
                forward_stream(local, input, output).await;
            }
        }
        Ok(())
    }
}

// SQLx closes its socket when the connection ends. A proxy must not keep the
// other half alive indefinitely after either end disconnects.
async fn forward_stream(
    local: &mut UnixStream,
    mut input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
) -> Option<u64> {
    use futures_util::future::{Either, select};

    let (mut local_input, mut local_output) = local.split();
    let download = Box::pin(tokio::io::copy(&mut input, &mut local_output));
    let upload = Box::pin(tokio::io::copy(&mut local_input, &mut output));
    match select(download, upload).await {
        Either::Left((result, _)) => result.ok(),
        Either::Right(_) => None,
    }
}

fn authority(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

async fn socks5(remote: &mut TcpStream, host: &str, port: u16) -> io::Result<()> {
    remote.write_all(&[5, 1, 0]).await?;
    let mut response = [0; 2];
    remote.read_exact(&mut response).await?;
    if response != [5, 0] {
        return Err(io::Error::other(
            "SOCKS5 не поддерживает подключение без авторизации",
        ));
    }
    let mut request = vec![5, 1, 0];
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            request.push(1);
            request.extend(ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            request.push(4);
            request.extend(ip.octets());
        }
        Err(_) => {
            request.extend([3, host.len() as u8]);
            request.extend(host.as_bytes());
        }
    }
    request.extend(port.to_be_bytes());
    remote.write_all(&request).await?;

    let mut reply = [0; 4];
    remote.read_exact(&mut reply).await?;
    if reply[0] != 5 || reply[2] != 0 {
        return Err(io::Error::other("Некорректный ответ SOCKS5"));
    }
    if reply[1] != 0 {
        return Err(io::Error::other(format!(
            "SOCKS5 отклонил запрос (код {})",
            reply[1]
        )));
    }
    let length = match reply[3] {
        1 => 4,
        3 => usize::from(remote.read_u8().await?),
        4 => 16,
        _ => return Err(io::Error::other("Некорректный адрес в ответе SOCKS5")),
    };
    let mut address = [0; 257];
    remote.read_exact(&mut address[..length + 2]).await?;
    Ok(())
}

async fn http_connect(remote: &mut TcpStream, authority: &str) -> io::Result<()> {
    let request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    remote.write_all(request.as_bytes()).await?;
    // CONNECT permits immediate tunnel bytes after the headers (e.g. MySQL greeting).
    let mut headers = Vec::new();
    while headers.len() < 16 * 1024 {
        headers.push(remote.read_u8().await?);
        if headers.ends_with(b"\r\n\r\n") {
            let line = headers
                .split(|byte| *byte == b'\r')
                .next()
                .unwrap_or_default();
            let mut fields = line.split(|byte| *byte == b' ');
            let version = fields.next();
            let code = fields
                .next()
                .filter(|code| code.len() == 3 && code.iter().all(u8::is_ascii_digit));
            let Some(code) = code.filter(|_| matches!(version, Some(b"HTTP/1.0" | b"HTTP/1.1")))
            else {
                return Err(io::Error::other("Некорректный ответ HTTP CONNECT"));
            };
            if code[0] != b'2' {
                return Err(io::Error::other(format!(
                    "HTTP CONNECT отклонён (код {})",
                    String::from_utf8_lossy(code)
                )));
            }
            return Ok(());
        }
    }
    Err(io::Error::other("Слишком длинные заголовки HTTP CONNECT"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{EnvironmentTag, TlsMode};
    use tokio::net::TcpListener;

    #[test]
    fn transport_proxy_protocols_preserve_immediate_payload_and_remove_socket() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(8), async {
                    for socks in [true, false] {
                        let proxy = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                        let gateway = "127.0.0.1".to_owned();
                        let port = proxy.local_addr().unwrap().port();
                        let transport = if socks {
                            Transport::Socks5 { host: gateway, port }
                        } else {
                            Transport::HttpConnect { host: gateway, port }
                        };
                        let profile = ConnectionProfile {
                            id: "relay-test".into(),
                            name: "relay-test".into(),
                            kind: DatabaseKind::PostgreSql,
                            host: "database.internal".into(),
                            port: 5432,
                            database: String::new(),
                            username: "test".into(),
                            environment: EnvironmentTag::Local,
                            color: String::new(),
                            read_only: true,
                            tls_mode: TlsMode::Disabled,
                            password_saved: false,
                            gopass_entry: None,
                            tls_files: Default::default(),
                            native_tools_dir: None,
                            transport,
                        };
                        let server = tokio::spawn(async move {
                            let (mut stream, _) = proxy.accept().await.unwrap();
                            if socks {
                                let mut greeting = [0; 3];
                                stream.read_exact(&mut greeting).await.unwrap();
                                assert_eq!(greeting, [5, 1, 0]);
                                stream.write_all(&[5, 0]).await.unwrap();
                                let mut request = [0; 24];
                                stream.read_exact(&mut request).await.unwrap();
                                assert_eq!(&request[..5], &[5, 1, 0, 3, 17]);
                                assert_eq!(&request[5..22], b"database.internal");
                                assert_eq!(&request[22..], &5432_u16.to_be_bytes());
                                stream
                                    .write_all(b"\x05\x00\x00\x01\x00\x00\x00\x00\x00\x00hello")
                                    .await
                                    .unwrap();
                            } else {
                                let expected = b"CONNECT database.internal:5432 HTTP/1.1\r\nHost: database.internal:5432\r\n\r\n";
                                let mut request = vec![0; expected.len()];
                                stream.read_exact(&mut request).await.unwrap();
                                assert_eq!(request, expected);
                                stream
                                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\nhello")
                                    .await
                                    .unwrap();
                            }
                            assert_eq!(stream.read_u8().await.unwrap(), 42);
                        });
                        let relay = Relay::start(&profile).unwrap().unwrap();
                        let mut client = UnixStream::connect(&relay.socket).await.unwrap();
                        let mut greeting = [0; 5];
                        client.read_exact(&mut greeting).await.unwrap();
                        assert_eq!(&greeting, b"hello");
                        client.write_u8(42).await.unwrap();
                        server.await.unwrap();
                        let directory = relay.directory.clone();
                        drop(relay);
                        assert!(!directory.exists());
                    }
                })
                .await
                .unwrap();
            });
    }

    #[test]
    fn transport_refuses_http_authentication_and_socks5_authentication() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                for socks in [true, false] {
                    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                    let mut client = TcpStream::connect(listener.local_addr().unwrap())
                        .await
                        .unwrap();
                    let (mut server, _) = listener.accept().await.unwrap();
                    let response: &[u8] = if socks {
                        &[5, 2]
                    } else {
                        b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n"
                    };
                    server.write_all(response).await.unwrap();
                    let result = if socks {
                        socks5(&mut client, "db.test", 5432).await
                    } else {
                        http_connect(&mut client, "db.test:5432").await
                    };
                    let error = result.unwrap_err().to_string();
                    assert!(error.contains(if socks { "SOCKS5" } else { "407" }));
                }
            });
    }
}
