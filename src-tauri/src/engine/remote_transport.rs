//! Read-only FTP, FTPS and SFTP sources. This module intentionally does not
//! borrow HTTP's validators: remote sizes are advisory metadata.

use std::{
    future::Future,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};

use russh::{
    client::{self, Handle},
    keys::{self, agent::AgentIdentity, key::PrivateKeyWithHashAlg, PublicKeyOrCertificate},
};
use russh_sftp::client::{Config as SftpConfig, SftpSession};
use suppaftp::{
    tokio::{
        AsyncFtpStream, AsyncRustlsConnector, AsyncRustlsFtpStream, ImplAsyncFtpStream,
        TokioTlsStream,
    },
    types::FileType,
    Status,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
use url::Url;

const CHUNK_BYTES: usize = 128 * 1024;
const CHANNEL_SLOTS: usize = 4;
// russh-sftp already pipelines read requests. Increase its bounded window
// rather than opening extra SSH connections on high-latency links. Typical
// servers answer with 32-64 KiB packets; 64 requests keep 2-4 MiB in flight.
const SFTP_READ_AHEAD: usize = 64;
// Bounded aggregate pressure for manually configured parallel SFTP transfers:
// at most 256 * 256 KiB = 64 MiB of negotiated response packet capacity.
const SFTP_PENDING_READ_BUDGET: usize = 256;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub(crate) struct RemoteInfo {
    pub total_bytes: Option<u64>,
    /// Whether this protocol/server supports seeking. This is never a
    /// representation validator; callers must independently establish identity.
    pub supports_resume: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    Ftp,
    FtpsExplicit,
    FtpsImplicit,
    Sftp,
}

#[derive(Debug, Clone)]
struct Target {
    protocol: Protocol,
    host: String,
    port: u16,
    path: String,
    user: String,
    password: Option<String>,
}

pub(crate) fn is_remote_url(url: &Url) -> bool {
    matches!(url.scheme(), "ftp" | "ftps" | "ftpes" | "sftp")
}

pub(crate) fn validate_url(url: &Url) -> Result<(), String> {
    Target::parse(url).map(|_| ())
}

impl Target {
    fn parse(url: &Url) -> Result<Self, String> {
        if url.as_str().len() > 8192 {
            return Err("Remote source URL is too long.".into());
        }
        let host = match url.host() {
            Some(url::Host::Domain(domain)) if !domain.is_empty() => domain.to_owned(),
            Some(url::Host::Ipv4(address)) => address.to_string(),
            Some(url::Host::Ipv6(address)) => address.to_string(),
            _ => return Err("Remote source must specify a server.".into()),
        };
        if url.query().is_some() || url.fragment().is_some() {
            return Err("Remote source cannot contain a query or fragment.".into());
        }
        let protocol = match url.scheme() {
            "ftp" => Protocol::Ftp,
            "ftps" | "ftpes" if url.port() == Some(990) => Protocol::FtpsImplicit,
            "ftps" | "ftpes" => Protocol::FtpsExplicit,
            "sftp" => Protocol::Sftp,
            _ => return Err("Unsupported remote transfer protocol.".into()),
        };
        let port = url.port().unwrap_or(match protocol {
            Protocol::Ftp | Protocol::FtpsExplicit => 21,
            Protocol::FtpsImplicit => 990,
            Protocol::Sftp => 22,
        });
        let path = decode_field(url.path())?;
        if path.is_empty() || path == "/" || path.ends_with('/') {
            return Err("Remote source must name a file.".into());
        }
        let user = decode_field(url.username())?;
        let password = url.password().map(decode_field).transpose()?;
        if protocol == Protocol::Sftp && user.is_empty() {
            return Err("SFTP source requires a username.".into());
        }
        Ok(Self {
            protocol,
            host,
            port,
            path,
            user,
            password,
        })
    }
}

fn decode_field(value: &str) -> Result<String, String> {
    let decoded = percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .map_err(|_| "Remote URL contains invalid UTF-8.".to_string())?;
    if decoded.chars().any(char::is_control) {
        return Err("Remote URL contains a forbidden control character.".into());
    }
    Ok(decoded.into_owned())
}

async fn guarded<T, E>(
    cancel: &CancellationToken,
    deadline: Duration,
    error: &'static str,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("Remote transfer cancelled.".into()),
        result = tokio::time::timeout(deadline, future) => match result {
            Ok(Ok(value)) => Ok(value),
            _ => Err(error.into()),
        },
    }
}

fn tls_connector() -> AsyncRustlsConnector {
    // Certificate-chain and hostname verification must never be bypassed.
    // Keep one immutable certificate store for all FTPS sessions instead of
    // reconstructing public roots for each parallel range and retry.
    static CONFIG: OnceLock<Arc<suppaftp::tokio_rustls::rustls::ClientConfig>> = OnceLock::new();
    let config = CONFIG.get_or_init(|| {
        let mut roots = suppaftp::tokio_rustls::rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Arc::new(
            suppaftp::tokio_rustls::rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    });
    AsyncRustlsConnector::from(suppaftp::tokio_rustls::TlsConnector::from(config.clone()))
}

fn sftp_read_ahead(concurrent_streams: usize, remaining_bytes: Option<u64>) -> usize {
    let ceiling =
        (SFTP_PENDING_READ_BUDGET / concurrent_streams.clamp(1, 32)).clamp(8, SFTP_READ_AHEAD);
    // The SFTP library speculates this many READs once it learns the server's
    // packet size. Do not send dozens of needless EOF requests for small files
    // or unknown-length sources; preserve a large window for bulk downloads.
    remaining_bytes.map_or(ceiling.min(16), |bytes| {
        ceiling.min(bytes.div_ceil(32 * 1024).clamp(1, ceiling as u64) as usize)
    })
}

enum FtpSession {
    Plain(AsyncFtpStream),
    Secure(AsyncRustlsFtpStream),
}

async fn open_ftp(target: &Target, cancel: &CancellationToken) -> Result<FtpSession, String> {
    let address = (target.host.as_str(), target.port);
    match target.protocol {
        Protocol::Ftp => {
            let mut stream = guarded(
                cancel,
                CONNECT_TIMEOUT,
                "FTP connection failed.",
                AsyncFtpStream::connect(address),
            )
            .await?;
            setup_ftp(&mut stream, target, cancel).await?;
            Ok(FtpSession::Plain(stream))
        }
        Protocol::FtpsExplicit | Protocol::FtpsImplicit => {
            let connector = tls_connector();
            let mut stream = if target.protocol == Protocol::FtpsImplicit {
                guarded(
                    cancel,
                    CONNECT_TIMEOUT,
                    "FTPS TLS connection or certificate verification failed.",
                    AsyncRustlsFtpStream::connect_secure_implicit(address, connector, &target.host),
                )
                .await?
            } else {
                let plain = guarded(
                    cancel,
                    CONNECT_TIMEOUT,
                    "FTPS connection failed.",
                    AsyncRustlsFtpStream::connect(address),
                )
                .await?;
                guarded(
                    cancel,
                    CONNECT_TIMEOUT,
                    "FTPS TLS negotiation or certificate verification failed.",
                    plain.into_secure(connector, &target.host),
                )
                .await?
            };
            // Implicit TLS starts encrypted, but still needs private data-channel
            // protection. Explicit TLS sets PBSZ/PROT in into_secure itself.
            if target.protocol == Protocol::FtpsImplicit {
                guarded(
                    cancel,
                    IO_TIMEOUT,
                    "FTPS private data-channel negotiation failed.",
                    stream.custom_command("PBSZ 0", &[Status::CommandOk]),
                )
                .await?;
                guarded(
                    cancel,
                    IO_TIMEOUT,
                    "FTPS private data-channel negotiation failed.",
                    stream.custom_command("PROT P", &[Status::CommandOk]),
                )
                .await?;
            }
            setup_ftp(&mut stream, target, cancel).await?;
            Ok(FtpSession::Secure(stream))
        }
        Protocol::Sftp => Err("SFTP cannot use the FTP transport.".into()),
    }
}

async fn setup_ftp<T: TokioTlsStream + Send>(
    ftp: &mut ImplAsyncFtpStream<T>,
    target: &Target,
    cancel: &CancellationToken,
) -> Result<(), String> {
    // Never use the IP returned by an FTP server's PASV response to establish
    // an unrelated outbound connection.
    ftp.set_passive_nat_workaround(true);
    let user = if target.user.is_empty() {
        "anonymous"
    } else {
        target.user.as_str()
    };
    let password = target
        .password
        .as_deref()
        .unwrap_or(if target.user.is_empty() {
            "anonymous@"
        } else {
            ""
        });
    guarded(
        cancel,
        IO_TIMEOUT,
        "FTP authentication failed.",
        ftp.login(user, password),
    )
    .await?;
    guarded(
        cancel,
        IO_TIMEOUT,
        "FTP binary transfer negotiation failed.",
        ftp.transfer_type(FileType::Binary),
    )
    .await
}

async fn probe_ftp<T: TokioTlsStream + Send>(
    ftp: &mut ImplAsyncFtpStream<T>,
    target: &Target,
    cancel: &CancellationToken,
    check_resume: bool,
) -> Result<RemoteInfo, String> {
    let total_bytes = guarded(
        cancel,
        IO_TIMEOUT,
        "FTP size lookup failed.",
        ftp.size(&target.path),
    )
    .await
    .ok()
    .map(|size| size as u64);
    // REST is useful only with a trusted expected hash. Skipping it for
    // ordinary single-stream transfers saves a round-trip and avoids sending
    // restart markers to servers that do not support them.
    let supports_resume = if check_resume && total_bytes.is_some_and(|bytes| bytes > 0) {
        guarded(
            cancel,
            IO_TIMEOUT,
            "FTP restart negotiation failed.",
            ftp.resume_transfer(0),
        )
        .await
        .is_ok()
    } else {
        false
    };
    if cancel.is_cancelled() {
        return Err("Remote transfer cancelled.".into());
    }
    Ok(RemoteInfo {
        total_bytes,
        supports_resume,
    })
}

#[derive(Clone)]
struct KnownHost {
    hostname: String,
    port: u16,
    rejected: Arc<AtomicBool>,
}

impl client::Handler for KnownHost {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // Certificates require full CA/principal verification; accepting only
        // a key extracted from a certificate would bypass that verification.
        let trusted = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                keys::check_known_hosts(&self.hostname, self.port, key).unwrap_or(false)
            }
            PublicKeyOrCertificate::Certificate(_) => false,
        };
        self.rejected.store(!trusted, Ordering::Release);
        Ok(trusted)
    }
}

async fn connect_sftp(
    target: &Target,
    cancel: &CancellationToken,
    concurrent_streams: usize,
    remaining_bytes: Option<u64>,
) -> Result<(Handle<KnownHost>, SftpSession), String> {
    let rejected = Arc::new(AtomicBool::new(false));
    let handler = KnownHost {
        hostname: target.host.clone(),
        port: target.port,
        rejected: rejected.clone(),
    };
    let mut config = client::Config::default();
    config.nodelay = true;
    let connection = guarded(
        cancel,
        CONNECT_TIMEOUT,
        "SFTP connection or host-key verification failed.",
        client::connect(
            Arc::new(config),
            (target.host.as_str(), target.port),
            handler,
        ),
    )
    .await;
    let mut ssh = match connection {
        Ok(ssh) => ssh,
        Err(_error) if rejected.load(Ordering::Acquire) => {
            return Err(
                "SFTP server host key is missing, changed or untrusted in ~/.ssh/known_hosts."
                    .into(),
            );
        }
        Err(error) => return Err(error),
    };
    let authenticated = if let Some(password) = target.password.as_deref() {
        guarded(
            cancel,
            IO_TIMEOUT,
            "SFTP password authentication failed.",
            ssh.authenticate_password(&target.user, password),
        )
        .await?
        .success()
    } else {
        try_agent_auth(&mut ssh, &target.user, cancel).await
            || try_identity_files(&mut ssh, &target.user, cancel).await
    };
    if !authenticated {
        return Err(
            "SFTP authentication failed. Supply a password or a configured SSH agent/key.".into(),
        );
    }
    let channel = guarded(
        cancel,
        IO_TIMEOUT,
        "SFTP channel could not be opened.",
        ssh.channel_open_session(),
    )
    .await?;
    guarded(
        cancel,
        IO_TIMEOUT,
        "SFTP subsystem is unavailable.",
        channel.request_subsystem(true, "sftp"),
    )
    .await?;
    let sftp_config = SftpConfig {
        max_concurrent_reads: sftp_read_ahead(concurrent_streams, remaining_bytes),
        ..SftpConfig::default()
    };
    let sftp = guarded(
        cancel,
        IO_TIMEOUT,
        "SFTP subsystem negotiation failed.",
        SftpSession::new_with_config(channel.into_stream(), sftp_config),
    )
    .await?;
    sftp.set_timeout(30);
    Ok((ssh, sftp))
}

async fn try_agent_auth(
    ssh: &mut Handle<KnownHost>,
    username: &str,
    cancel: &CancellationToken,
) -> bool {
    #[cfg(unix)]
    {
        if let Ok(agent) = guarded(
            cancel,
            CONNECT_TIMEOUT,
            "SSH agent is unavailable.",
            keys::agent::client::AgentClient::connect_env(),
        )
        .await
        {
            return authenticate_agent(ssh, username, agent, cancel).await;
        }
    }
    #[cfg(windows)]
    {
        let socket = std::env::var("SSH_AUTH_SOCK")
            .ok()
            .filter(|socket| socket.starts_with(r"\\.\pipe\"))
            .unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".to_owned());
        if let Ok(agent) = guarded(
            cancel,
            CONNECT_TIMEOUT,
            "SSH agent is unavailable.",
            keys::agent::client::AgentClient::connect_named_pipe(socket),
        )
        .await
        {
            if authenticate_agent(ssh, username, agent, cancel).await {
                return true;
            }
        }
        if let Ok(agent) = guarded(
            cancel,
            CONNECT_TIMEOUT,
            "Pageant is unavailable.",
            keys::agent::client::AgentClient::connect_pageant(),
        )
        .await
        {
            return authenticate_agent(ssh, username, agent, cancel).await;
        }
    }
    false
}

async fn authenticate_agent<S>(
    ssh: &mut Handle<KnownHost>,
    username: &str,
    mut agent: keys::agent::client::AgentClient<S>,
    cancel: &CancellationToken,
) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    let Ok(identities) = guarded(
        cancel,
        IO_TIMEOUT,
        "SSH agent identities unavailable.",
        agent.request_identities(),
    )
    .await
    else {
        return false;
    };
    for identity in identities.into_iter().take(16) {
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        let hash = if key.algorithm().is_rsa() {
            match guarded(
                cancel,
                IO_TIMEOUT,
                "Could not negotiate a safe RSA algorithm.",
                ssh.best_supported_rsa_hash(),
            )
            .await
            {
                Ok(Some(Some(hash))) => Some(hash),
                _ => continue,
            }
        } else {
            None
        };
        if let Ok(auth) = guarded(
            cancel,
            IO_TIMEOUT,
            "SSH agent authentication failed.",
            ssh.authenticate_publickey_with(username, key, hash, &mut agent),
        )
        .await
        {
            if auth.success() {
                return true;
            }
        }
    }
    false
}

fn ssh_home() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.map(PathBuf::from)
}

async fn try_identity_files(
    ssh: &mut Handle<KnownHost>,
    username: &str,
    cancel: &CancellationToken,
) -> bool {
    let Some(home) = ssh_home() else {
        return false;
    };
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        if cancel.is_cancelled() {
            return false;
        }
        let Ok(key) = keys::load_secret_key(home.join(".ssh").join(name), None) else {
            continue;
        };
        let hash = if key.algorithm().is_rsa() {
            match guarded(
                cancel,
                IO_TIMEOUT,
                "Could not negotiate a safe RSA algorithm.",
                ssh.best_supported_rsa_hash(),
            )
            .await
            {
                Ok(Some(Some(hash))) => Some(hash),
                _ => continue,
            }
        } else {
            None
        };
        let auth = guarded(
            cancel,
            IO_TIMEOUT,
            "SFTP public-key authentication failed.",
            ssh.authenticate_publickey(username, PrivateKeyWithHashAlg::new(Arc::new(key), hash)),
        )
        .await;
        if auth.is_ok_and(|result| result.success()) {
            return true;
        }
    }
    false
}

pub(crate) async fn probe(
    url: &Url,
    cancel: &CancellationToken,
    check_resume: bool,
) -> Result<RemoteInfo, String> {
    if cancel.is_cancelled() {
        return Err("Remote transfer cancelled.".into());
    }
    let target = Target::parse(url)?;
    match target.protocol {
        Protocol::Ftp | Protocol::FtpsExplicit | Protocol::FtpsImplicit => {
            match open_ftp(&target, cancel).await? {
                FtpSession::Plain(mut ftp) => {
                    probe_ftp(&mut ftp, &target, cancel, check_resume).await
                }
                FtpSession::Secure(mut ftp) => {
                    probe_ftp(&mut ftp, &target, cancel, check_resume).await
                }
            }
        }
        Protocol::Sftp => {
            let (_ssh, sftp) = connect_sftp(&target, cancel, 1, Some(0)).await?;
            let attributes = guarded(
                cancel,
                IO_TIMEOUT,
                "SFTP metadata lookup failed.",
                sftp.metadata(&target.path),
            )
            .await?;
            if attributes.is_dir() {
                return Err("SFTP path names a directory, not a file.".into());
            }
            Ok(RemoteInfo {
                total_bytes: attributes.size,
                supports_resume: true,
            })
        }
    }
}

async fn send_failure(tx: &mpsc::Sender<Result<Vec<u8>, String>>, failure: String) {
    // A slow receiver must observe the final server error even after all
    // payload chunks. Dropping an error after a timeout can falsely publish a
    // successful FTP transfer; send() releases immediately on receiver drop.
    let _ = tx.send(Err(failure)).await;
}

async fn send_chunk(
    tx: &mpsc::Sender<Result<Vec<u8>, String>>,
    cancel: &CancellationToken,
    chunk: Vec<u8>,
) -> Result<(), String> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("Remote transfer cancelled.".into()),
        _ = tx.closed() => Err("Remote transfer consumer closed.".into()),
        result = tx.send(Ok(chunk)) => result.map_err(|_| "Remote transfer consumer closed.".into()),
    }
}

async fn transfer_ftp<T: TokioTlsStream + Send>(
    mut ftp: ImplAsyncFtpStream<T>,
    path: String,
    offset: u64,
    advertised_size: Option<u64>,
    cancel: CancellationToken,
    tx: mpsc::Sender<Result<Vec<u8>, String>>,
) -> Result<(), String> {
    // The initial probe already checked this file. Repeating SIZE for every
    // parallel range costs a control-channel RTT on every authenticated
    // connection; the final byte count and expected SHA-256 remain authoritative.
    let known_size = if let Some(size) = advertised_size {
        Some(size)
    } else {
        guarded(
            &cancel,
            IO_TIMEOUT,
            "FTP file-size lookup failed.",
            ftp.size(&path),
        )
        .await
        .ok()
        .map(|value| value as u64)
    };
    if known_size.is_some_and(|total| offset > total) {
        return Err("Requested FTP offset exceeds the remote file.".into());
    }
    if offset > 0 {
        let offset = usize::try_from(offset)
            .map_err(|_| "Requested FTP offset is too large.".to_string())?;
        guarded(
            &cancel,
            IO_TIMEOUT,
            "FTP server refused to resume from the requested offset.",
            ftp.resume_transfer(offset),
        )
        .await?;
    }
    let mut data = guarded(
        &cancel,
        IO_TIMEOUT,
        "FTP file retrieval could not start.",
        ftp.retr_as_stream(path),
    )
    .await?;
    let mut received = 0u64;
    // suppaftp's data-stream adapter expects the conventional fixed read
    // buffer for reliable EOF and terminal reply handling on Windows.
    let mut buf = vec![0; CHUNK_BYTES];
    loop {
        let n = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("Remote transfer cancelled.".into()),
            _ = tx.closed() => return Ok(()),
            result = tokio::time::timeout(IO_TIMEOUT, data.read(&mut buf)) => match result {
                Ok(Ok(n)) => n,
                Ok(Err(_error)) => {
                    // Even when Windows reports a data-socket reset, the
                    // control connection may contain the server's final 426.
                    // Surface that failure while still rejecting the body if
                    // the server instead replies 226.
                    return match guarded(
                        &cancel,
                        IO_TIMEOUT,
                        "FTP server did not confirm a successful transfer.",
                        data.finish(),
                    )
                    .await
                    {
                        Err(failure) => Err(failure),
                        Ok(()) => Err("FTP data connection failed.".into()),
                    };
                }
                Err(_) => return Err("FTP data connection stalled.".into()),
            }
        };
        if n == 0 {
            break;
        }
        received += n as u64;
        if known_size.is_some_and(|size| offset.saturating_add(received) > size) {
            return Err("FTP source grew or sent extra bytes during the transfer.".into());
        }
        // Keep the established FTP fixed-buffer read path; SFTP separately
        // uses move-only read buffers where its protocol adapter supports it.
        if send_chunk(&tx, &cancel, buf[..n].to_vec()).await.is_err() {
            return if cancel.is_cancelled() {
                Err("Remote transfer cancelled.".into())
            } else {
                Ok(())
            };
        }
    }
    guarded(
        &cancel,
        IO_TIMEOUT,
        "FTP server did not confirm a successful transfer.",
        data.finish(),
    )
    .await?;
    if known_size.is_some_and(|size| offset.saturating_add(received) != size) {
        return Err("FTP source ended before the announced file length.".into());
    }
    Ok(())
}

async fn transfer_sftp(
    sftp: SftpSession,
    path: String,
    offset: u64,
    advertised_size: Option<u64>,
    cancel: CancellationToken,
    tx: mpsc::Sender<Result<Vec<u8>, String>>,
) -> Result<(), String> {
    let mut file = guarded(
        &cancel,
        IO_TIMEOUT,
        "SFTP file could not be opened.",
        sftp.open(path),
    )
    .await?;
    // Avoid an FSTAT round-trip on every segment when the initial probe has
    // already checked the type and returned a length.
    let known_size = if let Some(size) = advertised_size {
        Some(size)
    } else {
        let metadata = guarded(
            &cancel,
            IO_TIMEOUT,
            "SFTP file metadata lookup failed.",
            file.metadata(),
        )
        .await?;
        if metadata.is_dir() {
            return Err("SFTP source is a directory.".into());
        }
        metadata.size
    };
    if known_size.is_some_and(|size| offset > size) {
        return Err("Requested SFTP offset exceeds the remote file.".into());
    }
    if offset > 0 {
        guarded(
            &cancel,
            IO_TIMEOUT,
            "SFTP seek failed.",
            file.seek(std::io::SeekFrom::Start(offset)),
        )
        .await?;
    }
    let mut received = 0u64;
    let mut buf = Vec::with_capacity(CHUNK_BYTES);
    loop {
        let n = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("Remote transfer cancelled.".into()),
            _ = tx.closed() => return Ok(()),
            result = tokio::time::timeout(IO_TIMEOUT, file.read_buf(&mut buf)) => match result {
                Ok(Ok(n)) => n,
                _ => return Err("SFTP data connection failed or stalled.".into()),
            }
        };
        if n == 0 {
            break;
        }
        received += n as u64;
        if known_size.is_some_and(|size| offset.saturating_add(received) > size) {
            return Err("SFTP source grew or sent extra bytes during transfer.".into());
        }
        if send_chunk(&tx, &cancel, buf).await.is_err() {
            return if cancel.is_cancelled() {
                Err("Remote transfer cancelled.".into())
            } else {
                Ok(())
            };
        }
        buf = Vec::with_capacity(CHUNK_BYTES);
    }
    if known_size.is_some_and(|size| offset.saturating_add(received) != size) {
        return Err("SFTP source ended before the announced file length.".into());
    }
    guarded(&cancel, IO_TIMEOUT, "SFTP file close failed.", file.close()).await?;
    Ok(())
}

pub(crate) async fn stream(
    url: &Url,
    offset: u64,
    advertised_size: Option<u64>,
    concurrent_streams: usize,
    remaining_bytes: Option<u64>,
    cancel: &CancellationToken,
) -> Result<mpsc::Receiver<Result<Vec<u8>, String>>, String> {
    if cancel.is_cancelled() {
        return Err("Remote transfer cancelled.".into());
    }
    let target = Target::parse(url)?;
    let (tx, rx) = mpsc::channel(CHANNEL_SLOTS);
    let cancel = cancel.clone();
    match target.protocol {
        Protocol::Ftp | Protocol::FtpsExplicit | Protocol::FtpsImplicit => {
            let ftp = open_ftp(&target, &cancel).await?;
            tokio::spawn(async move {
                let result = match ftp {
                    FtpSession::Plain(ftp) => {
                        transfer_ftp(
                            ftp,
                            target.path,
                            offset,
                            advertised_size,
                            cancel,
                            tx.clone(),
                        )
                        .await
                    }
                    FtpSession::Secure(ftp) => {
                        transfer_ftp(
                            ftp,
                            target.path,
                            offset,
                            advertised_size,
                            cancel,
                            tx.clone(),
                        )
                        .await
                    }
                };
                if let Err(error) = result {
                    send_failure(&tx, error).await;
                }
            });
        }
        Protocol::Sftp => {
            let (ssh, sftp) =
                connect_sftp(&target, &cancel, concurrent_streams, remaining_bytes).await?;
            tokio::spawn(async move {
                let result = transfer_sftp(
                    sftp,
                    target.path,
                    offset,
                    advertised_size,
                    cancel,
                    tx.clone(),
                )
                .await;
                // Keep the authenticated SSH session alive until SFTP is done.
                drop(ssh);
                if let Err(error) = result {
                    send_failure(&tx, error).await;
                }
            });
        }
    }
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh_sftp::protocol::{
        Attrs as SftpAttrs, Data as SftpData, FileAttributes as SftpFileAttributes,
        Handle as SftpHandle, OpenFlags, Status as SftpStatus, StatusCode as SftpStatusCode,
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };

    struct SftpFixture {
        bytes: Arc<Vec<u8>>,
        fstat_count: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl russh_sftp::server::Handler for SftpFixture {
        type Error = SftpStatusCode;

        fn unimplemented(&self) -> Self::Error {
            SftpStatusCode::OpUnsupported
        }

        async fn open(
            &mut self,
            id: u32,
            filename: String,
            _flags: OpenFlags,
            _attrs: SftpFileAttributes,
        ) -> Result<SftpHandle, Self::Error> {
            if filename != "/file.bin" {
                return Err(SftpStatusCode::NoSuchFile);
            }
            Ok(SftpHandle {
                id,
                handle: "fixture-file".into(),
            })
        }

        async fn read(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            len: u32,
        ) -> Result<SftpData, Self::Error> {
            if handle != "fixture-file" {
                return Err(SftpStatusCode::Failure);
            }
            let start = usize::try_from(offset).map_err(|_| SftpStatusCode::Eof)?;
            if start >= self.bytes.len() {
                return Err(SftpStatusCode::Eof);
            }
            // A small per-request result exercises the library's short-read
            // handling and bounded in-flight read queue.
            let end = start
                .saturating_add((len as usize).min(32 * 1024))
                .min(self.bytes.len());
            Ok(SftpData {
                id,
                data: self.bytes[start..end].to_vec(),
            })
        }

        async fn fstat(&mut self, id: u32, handle: String) -> Result<SftpAttrs, Self::Error> {
            if handle != "fixture-file" {
                return Err(SftpStatusCode::Failure);
            }
            self.fstat_count.fetch_add(1, Ordering::Relaxed);
            Ok(SftpAttrs {
                id,
                attrs: SftpFileAttributes {
                    size: Some(self.bytes.len() as u64),
                    ..Default::default()
                },
            })
        }

        async fn close(&mut self, id: u32, _handle: String) -> Result<SftpStatus, Self::Error> {
            Ok(SftpStatus {
                id,
                status_code: SftpStatusCode::Ok,
                error_message: String::new(),
                language_tag: String::new(),
            })
        }
    }

    async fn sftp_fixture(bytes: Vec<u8>) -> (SftpSession, Arc<std::sync::atomic::AtomicUsize>) {
        let (client, server) = tokio::io::duplex(2 * 1024 * 1024);
        let fstat_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        russh_sftp::server::run(
            server,
            SftpFixture {
                bytes: Arc::new(bytes),
                fstat_count: fstat_count.clone(),
            },
        )
        .await;
        let session = SftpSession::new_with_config(
            client,
            SftpConfig {
                max_concurrent_reads: SFTP_READ_AHEAD,
                ..SftpConfig::default()
            },
        )
        .await
        .unwrap();
        session.set_timeout(30);
        (session, fstat_count)
    }

    #[derive(Clone, Default)]
    struct FtpFixtureOptions {
        deny_rest: bool,
        fail_final: bool,
        short_body: bool,
        stall: bool,
        commands: Option<Arc<std::sync::Mutex<Vec<String>>>>,
    }

    async fn respond(connection: &mut BufReader<TcpStream>, response: &str) {
        connection
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .unwrap();
    }

    async fn serve_ftp(socket: TcpStream, contents: Arc<Vec<u8>>, options: FtpFixtureOptions) {
        let mut connection = BufReader::new(socket);
        respond(&mut connection, "220 Loopback test server ready\r\n").await;
        let mut passive: Option<TcpListener> = None;
        let mut offset = 0usize;
        loop {
            let mut line = String::new();
            if connection.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let command = line.trim();
            if let Some(commands) = &options.commands {
                commands.lock().unwrap().push(command.to_owned());
            }
            let (verb, argument) = command.split_once(' ').unwrap_or((command, ""));
            match verb {
                "USER" if argument == "anonymous" => {
                    respond(&mut connection, "331 Password required\r\n").await
                }
                "USER" => respond(&mut connection, "530 User not found\r\n").await,
                "PASS" => respond(&mut connection, "230 Anonymous login okay\r\n").await,
                "TYPE" if argument == "I" => {
                    respond(&mut connection, "200 Binary mode okay\r\n").await
                }
                "SIZE" if argument == "/file.bin" => {
                    respond(&mut connection, &format!("213 {}\r\n", contents.len())).await
                }
                "REST" if options.deny_rest => {
                    respond(&mut connection, "502 REST unsupported\r\n").await
                }
                "REST" => {
                    offset = argument.parse::<usize>().unwrap();
                    respond(&mut connection, "350 Restart position accepted\r\n").await
                }
                "EPSV" | "PASV" => {
                    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let port = listener.local_addr().unwrap().port();
                    if verb == "PASV" {
                        respond(
                            &mut connection,
                            &format!(
                                "227 Entering Passive Mode (127,0,0,1,{},{})\r\n",
                                port / 256,
                                port % 256,
                            ),
                        )
                        .await;
                    } else {
                        respond(
                            &mut connection,
                            &format!("229 Entering Extended Passive Mode (|||{port}|)\r\n"),
                        )
                        .await;
                    }
                    passive = Some(listener);
                }
                "RETR" if argument == "/file.bin" => {
                    respond(&mut connection, "150 Sending binary file\r\n").await;
                    let (mut data, _) = passive.take().unwrap().accept().await.unwrap();
                    let chunk = &contents[offset..];
                    if options.stall {
                        let _ = data.write_all(&chunk[..chunk.len().min(CHUNK_BYTES)]).await;
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        return;
                    }
                    let bytes = if options.short_body {
                        &chunk[..chunk.len() / 2]
                    } else {
                        chunk
                    };
                    data.write_all(bytes).await.unwrap();
                    data.shutdown().await.unwrap();
                    // A real FTP server sends its terminal control reply as
                    // soon as its data stream finishes. Holding 226/426 until
                    // the client half-closes can deadlock with clients whose
                    // finish() waits for it. The client can also half-close
                    // its write side *before* reading the full body, so a
                    // read(0) here is not proof the payload was consumed.
                    respond(
                        &mut connection,
                        if options.fail_final {
                            "426 Connection closed; transfer aborted\r\n"
                        } else {
                            "226 Transfer complete\r\n"
                        },
                    )
                    .await;
                    // Keep the test's tiny loopback data socket alive long
                    // enough for Windows to deliver its buffered bytes/FIN,
                    // without incorrectly waiting for an early client FIN.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    drop(data);
                    offset = 0;
                }
                "QUIT" => {
                    respond(&mut connection, "221 Goodbye\r\n").await;
                    return;
                }
                _ => respond(&mut connection, "502 Command unsupported\r\n").await,
            }
        }
    }

    async fn ftp_fixture(contents: Vec<u8>, options: FtpFixtureOptions) -> (Url, JoinHandle<()>) {
        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!(
            "ftp://127.0.0.1:{}/file.bin",
            server.local_addr().unwrap().port()
        ))
        .unwrap();
        let contents = Arc::new(contents);
        let server_handle = tokio::spawn(async move {
            while let Ok((socket, _)) = server.accept().await {
                tokio::spawn(serve_ftp(socket, contents.clone(), options.clone()));
            }
        });
        (url, server_handle)
    }

    async fn collect_stream(
        mut receiver: mpsc::Receiver<Result<Vec<u8>, String>>,
    ) -> Result<(Vec<u8>, usize), String> {
        let mut output = Vec::new();
        let mut max_chunk = 0usize;
        // Windows loopback sockets can take several seconds to complete the
        // data half-close under test/CI contention. Production has its own
        // 30s I/O deadline; this test window must not undercut that handshake.
        while let Some(received) = tokio::time::timeout(Duration::from_secs(12), receiver.recv())
            .await
            .map_err(|_| format!("Transfer fixture timed out after {} bytes", output.len()))?
        {
            let data = received?;
            max_chunk = max_chunk.max(data.len());
            output.extend_from_slice(&data);
        }
        Ok((output, max_chunk))
    }

    #[test]
    fn remote_schemes_only_and_tls_modes_are_explicit() {
        for scheme in ["ftp", "ftps", "ftpes", "sftp"] {
            let url = Url::parse(&format!("{scheme}://user@example.org/file.bin")).unwrap();
            assert!(is_remote_url(&url));
        }
        for scheme in ["http", "https", "file", "gopher"] {
            let url = Url::parse(&format!("{scheme}://example.org/file.bin")).unwrap();
            assert!(!is_remote_url(&url));
            assert!(Target::parse(&url).is_err());
        }
        let explicit = Target::parse(&Url::parse("ftps://example.org/f").unwrap()).unwrap();
        assert_eq!(explicit.protocol, Protocol::FtpsExplicit);
        assert_eq!(explicit.port, 21);
        let implicit = Target::parse(&Url::parse("ftps://example.org:990/f").unwrap()).unwrap();
        assert_eq!(implicit.protocol, Protocol::FtpsImplicit);
        let ipv6 = Target::parse(&Url::parse("sftp://alice@[::1]/file.bin").unwrap()).unwrap();
        assert_eq!(ipv6.host, "::1");
    }

    #[test]
    fn malformed_paths_and_url_commands_are_rejected_before_network() {
        for source in [
            "ftp://example.org/",
            "sftp://example.org/file",
            "ftp://example.org/file?token=foo",
            "ftp://example.org/file#fragment",
            "ftp://example.org/file%0D%0APASS%20hacked",
            "sftp://alice@example.org/file%00suffix",
        ] {
            assert!(
                Target::parse(&Url::parse(source).unwrap()).is_err(),
                "{source}"
            );
        }
        let url = Url::parse("sftp://alice:p%40ssw%3Ard@example.org/a%20file.bin").unwrap();
        let target = Target::parse(&url).unwrap();
        assert_eq!(target.user, "alice");
        assert_eq!(target.password.as_deref(), Some("p@ssw:rd"));
        assert_eq!(target.path, "/a file.bin");
    }

    #[test]
    fn sftp_pipelining_balances_bandwidth_delay_and_memory_for_parallel_ranges() {
        // One connection can fill a high-RTT link; additional authenticated
        // ranges share the same aggregate 64 MiB packet-capacity budget.
        for (connections, requests_per_session) in [(1, 64), (4, 64), (8, 32), (16, 16), (32, 8)] {
            assert_eq!(
                sftp_read_ahead(connections, Some(128 * 1024 * 1024)),
                requests_per_session
            );
            assert!(
                connections * sftp_read_ahead(connections, Some(128 * 1024 * 1024))
                    <= SFTP_PENDING_READ_BUDGET
            );
        }
        assert_eq!(sftp_read_ahead(1, Some(1)), 1);
        assert_eq!(sftp_read_ahead(1, Some(40 * 1024)), 2);
        assert_eq!(sftp_read_ahead(1, Some(512 * 1024)), 16);
        assert_eq!(sftp_read_ahead(1, None), 16);
        assert_eq!(sftp_read_ahead(32, Some(1)), 1);
    }

    #[tokio::test]
    async fn cancelled_probe_and_stream_never_open_sockets() {
        let url = Url::parse("sftp://alice:secret@example.invalid/file.bin").unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(probe(&url, &cancel, true)
            .await
            .unwrap_err()
            .contains("cancelled"));
        assert!(stream(&url, 0, None, 1, None, &cancel)
            .await
            .unwrap_err()
            .contains("cancelled"));
    }

    #[tokio::test]
    async fn anonymous_ftp_probes_and_streams_bounded_chunks_with_resume() {
        let bytes = (0..(3 * CHUNK_BYTES + 73))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let (url, server) = ftp_fixture(bytes.clone(), FtpFixtureOptions::default()).await;
        let cancel = CancellationToken::new();
        let metadata = probe(&url, &cancel, true).await.unwrap();
        assert_eq!(metadata.total_bytes, Some(bytes.len() as u64));
        assert!(metadata.supports_resume);

        let (output, chunk_size) = collect_stream(
            stream(
                &url,
                0,
                metadata.total_bytes,
                1,
                metadata.total_bytes,
                &cancel,
            )
            .await
            .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(output, bytes);
        assert!(chunk_size <= CHUNK_BYTES);

        let offset = CHUNK_BYTES + 15;
        let (output, _) = collect_stream(
            stream(&url, offset as u64, metadata.total_bytes, 1, None, &cancel)
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(output, bytes[offset..]);
        server.abort();
    }

    #[tokio::test]
    async fn unverified_ftp_skips_restart_probe_and_reuses_advertised_length() {
        let bytes = (0..(CHUNK_BYTES * 2 + 37))
            .map(|index| (index % 211) as u8)
            .collect::<Vec<_>>();
        let commands = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (url, server) = ftp_fixture(
            bytes.clone(),
            FtpFixtureOptions {
                commands: Some(commands.clone()),
                ..Default::default()
            },
        )
        .await;
        let cancel = CancellationToken::new();
        let metadata = probe(&url, &cancel, false).await.unwrap();
        assert_eq!(metadata.total_bytes, Some(bytes.len() as u64));
        assert!(!metadata.supports_resume);

        let (output, _) = collect_stream(
            stream(
                &url,
                0,
                metadata.total_bytes,
                1,
                metadata.total_bytes,
                &cancel,
            )
            .await
            .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(output, bytes);
        let commands = commands.lock().unwrap();
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.starts_with("SIZE "))
                .count(),
            1
        );
        assert!(!commands.iter().any(|command| command.starts_with("REST ")));
        server.abort();
    }

    #[tokio::test]
    async fn sftp_reads_short_packets_and_avoids_duplicate_metadata_lookups() {
        let bytes = (0..(CHUNK_BYTES * 10 + 17))
            .map(|index| (index % 239) as u8)
            .collect::<Vec<_>>();
        let (session, fstat_count) = sftp_fixture(bytes.clone()).await;
        let cancel = CancellationToken::new();
        let (sender, receiver) = mpsc::channel(CHANNEL_SLOTS);
        let transfer = tokio::spawn(transfer_sftp(
            session,
            "/file.bin".into(),
            0,
            Some(bytes.len() as u64),
            cancel,
            sender,
        ));
        let (output, max_chunk) = collect_stream(receiver).await.unwrap();
        transfer.await.unwrap().unwrap();
        assert_eq!(output, bytes);
        assert!(max_chunk <= CHUNK_BYTES);
        assert_eq!(fstat_count.load(Ordering::Relaxed), 0);

        let offset = CHUNK_BYTES + 11;
        let (session, fstat_count) = sftp_fixture(bytes.clone()).await;
        let (sender, receiver) = mpsc::channel(CHANNEL_SLOTS);
        let transfer = tokio::spawn(transfer_sftp(
            session,
            "/file.bin".into(),
            offset as u64,
            None,
            CancellationToken::new(),
            sender,
        ));
        let (output, _) = collect_stream(receiver).await.unwrap();
        transfer.await.unwrap().unwrap();
        assert_eq!(output, bytes[offset..]);
        assert_eq!(fstat_count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn advertised_length_still_rejects_changed_ftp_source() {
        let bytes = vec![37; CHUNK_BYTES * 2 + 19];
        let (url, server) = ftp_fixture(bytes.clone(), FtpFixtureOptions::default()).await;
        let cancel = CancellationToken::new();
        let error = collect_stream(
            stream(&url, 0, Some(bytes.len() as u64 - 1), 1, None, &cancel)
                .await
                .unwrap(),
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("extra bytes"),
            "unexpected FTP error: {error}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn ftp_restart_refusal_and_final_reply_fail_without_false_completion() {
        let bytes = vec![71; CHUNK_BYTES * 2];
        let cancel = CancellationToken::new();
        let (url, server) = ftp_fixture(
            bytes.clone(),
            FtpFixtureOptions {
                deny_rest: true,
                ..Default::default()
            },
        )
        .await;
        assert!(!probe(&url, &cancel, true).await.unwrap().supports_resume);
        let error = collect_stream(stream(&url, 10, None, 1, None, &cancel).await.unwrap())
            .await
            .unwrap_err();
        assert!(error.contains("offset"));
        server.abort();

        let (url, server) = ftp_fixture(
            bytes.clone(),
            FtpFixtureOptions {
                fail_final: true,
                ..Default::default()
            },
        )
        .await;
        let error = collect_stream(stream(&url, 0, None, 1, None, &cancel).await.unwrap())
            .await
            .unwrap_err();
        assert!(error.contains("confirm"), "unexpected FTP error: {error}");
        server.abort();

        let (url, server) = ftp_fixture(
            bytes,
            FtpFixtureOptions {
                short_body: true,
                ..Default::default()
            },
        )
        .await;
        let error = collect_stream(stream(&url, 0, None, 1, None, &cancel).await.unwrap())
            .await
            .unwrap_err();
        assert!(error.contains("length"), "unexpected FTP error: {error}");
        server.abort();
    }

    #[tokio::test]
    async fn ftp_cancellation_interrupts_stalled_data_without_waiting_for_read_timeout() {
        let (url, server) = ftp_fixture(
            vec![4; CHUNK_BYTES * 4],
            FtpFixtureOptions {
                stall: true,
                ..Default::default()
            },
        )
        .await;
        let cancel = CancellationToken::new();
        let mut receiver = stream(&url, 0, None, 1, None, &cancel).await.unwrap();
        let chunk = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!chunk.is_empty());
        cancel.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(chunk) = receiver.recv().await {
                if let Err(error) = chunk {
                    return Some(error);
                }
            }
            None
        })
        .await
        .unwrap()
        .expect("cancelled transfer should report an error");
        assert!(outcome.contains("cancelled"));
        server.abort();
    }

    #[tokio::test]
    async fn error_waits_for_full_queue_and_slow_consumer() {
        let (tx, mut rx) = mpsc::channel(CHANNEL_SLOTS);
        for _ in 0..CHANNEL_SLOTS {
            tx.send(Ok(vec![1u8])).await.unwrap();
        }
        let reporter = tokio::spawn(async move {
            send_failure(&tx, "server rejected transfer".into()).await;
        });
        tokio::time::sleep(Duration::from_millis(5200)).await;
        for _ in 0..CHANNEL_SLOTS {
            assert_eq!(rx.recv().await.unwrap().unwrap(), vec![1u8]);
        }
        assert_eq!(
            rx.recv().await.unwrap().unwrap_err(),
            "server rejected transfer"
        );
        reporter.await.unwrap();
    }
}
