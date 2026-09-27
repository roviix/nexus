//! Claude 这一条通道自己的出站。
//!
//! 其它通道继续用网关里那个 reqwest。这里只服务打到 Anthropic 自己域名的请求：
//! 推理走 Claude Code 2.1.220 在 macOS 上抓到的 Node/OpenSSL ClientHello（ALPN
//! 只有 `http/1.1`，带 OCSP 和 SCT），换票和读资料走同一套密码套件，但不带
//! ALPN、OCSP、SCT。请求在这条 TLS 上按 HTTP/1.1 自己写，头的大小写才会原样
//! 到达上游。

use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use boring2::ssl::{SslConnector, SslCurve, SslMethod, SslVersion};
use boring2::x509::store::X509StoreBuilder;
use boring2::x509::X509;
use tokio::net::TcpStream;

use crate::http1;

pub use crate::http1::{Profile, Response, TransportError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

/// Claude Code 2.1.220 的密码套件，顺序按抓包。TLS 1.3 的三套写在最前：
/// BoringSSL 的 `set_cipher_list` 不会自己补上它们。
const CIPHERS: &str = "TLS_AES_128_GCM_SHA256:\
TLS_AES_256_GCM_SHA384:\
TLS_CHACHA20_POLY1305_SHA256:\
ECDHE-ECDSA-AES128-GCM-SHA256:\
ECDHE-RSA-AES128-GCM-SHA256:\
ECDHE-ECDSA-AES256-GCM-SHA384:\
ECDHE-RSA-AES256-GCM-SHA384:\
ECDHE-ECDSA-CHACHA20-POLY1305:\
ECDHE-RSA-CHACHA20-POLY1305:\
ECDHE-ECDSA-AES128-SHA:\
ECDHE-RSA-AES128-SHA:\
ECDHE-ECDSA-AES256-SHA:\
ECDHE-RSA-AES256-SHA:\
AES128-GCM-SHA256:\
AES256-GCM-SHA384:\
AES128-SHA:\
AES256-SHA";

const SIGALGS: &str = "ecdsa_secp256r1_sha256:\
rsa_pss_rsae_sha256:\
rsa_pkcs1_sha256:\
ecdsa_secp384r1_sha384:\
rsa_pss_rsae_sha384:\
rsa_pkcs1_sha384:\
rsa_pss_rsae_sha512:\
rsa_pkcs1_sha512:\
rsa_pkcs1_sha1";

const CURVES: &[SslCurve] = &[SslCurve::X25519, SslCurve::SECP256R1, SslCurve::SECP384R1];

/// `api.anthropic.com` 才换这套握手。供应商即使讲 Messages，域名不是这家就不动。
pub fn is_anthropic_api(base_url: &str) -> bool {
    let rest = base_url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = rest.split(['/', ':', '?', '#']).next().unwrap_or("").trim();
    host.eq_ignore_ascii_case("api.anthropic.com")
}

pub async fn request(
    profile: Profile,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Result<Response, TransportError> {
    let (host, port, path) = split_url(url).map_err(http1::fail)?;
    let deadline = match profile {
        Profile::Control => Some(Instant::now() + CONTROL_TIMEOUT),
        Profile::Inference => None,
    };
    let stream = connect(profile, &host, port, deadline).await?;
    http1::exchange(
        stream, profile, method, &host, port, &path, headers, body, deadline,
    )
    .await
}

fn split_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("只走 https：{url}"))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            let port = port
                .parse::<u16>()
                .map_err(|_| format!("端口无效：{port}"))?;
            (host.to_string(), port)
        }
        _ => (hostport.to_string(), 443),
    };
    if host.is_empty() {
        return Err("缺少主机名".into());
    }
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path
    };
    Ok((host, port, path))
}

async fn connect(
    profile: Profile,
    host: &str,
    port: u16,
    deadline: Option<Instant>,
) -> Result<tokio_boring2::SslStream<TcpStream>, TransportError> {
    let addr = resolve(host, port, deadline).await?;
    let connect = TcpStream::connect(addr);
    let tcp = bounded(connect, deadline, CONNECT_TIMEOUT, "连接 Anthropic 超时").await?;
    tcp.set_nodelay(true).ok();
    let mut conf = connector(profile)
        .configure()
        .map_err(|err| http1::fail(err.to_string()))?;
    conf.set_enable_ech_grease(false);
    let handshake = tokio_boring2::connect(conf, host, tcp);
    bounded(handshake, deadline, CONNECT_TIMEOUT, "TLS 握手超时")
        .await
        .map_err(|err| {
            if err.timeout {
                err
            } else {
                http1::fail(format!("TLS 握手失败：{}", err.message))
            }
        })
}

async fn resolve(
    host: &str,
    port: u16,
    deadline: Option<Instant>,
) -> Result<SocketAddr, TransportError> {
    let lookup = tokio::net::lookup_host((host, port));
    let mut addrs = bounded(lookup, deadline, CONNECT_TIMEOUT, "解析主机超时").await?;
    addrs
        .next()
        .ok_or_else(|| http1::fail(format!("解析不到 {host}")))
}

async fn bounded<F, T, E>(
    fut: F,
    deadline: Option<Instant>,
    cap: Duration,
    timeout_msg: &str,
) -> Result<T, TransportError>
where
    F: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut wait = cap;
    if let Some(deadline) = deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(http1::timed_out(timeout_msg));
        }
        wait = wait.min(left);
    }
    match tokio::time::timeout(wait, fut).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(http1::fail(err.to_string())),
        Err(_) => Err(http1::timed_out(timeout_msg)),
    }
}

fn connector(profile: Profile) -> &'static SslConnector {
    static INFERENCE: OnceLock<SslConnector> = OnceLock::new();
    static CONTROL: OnceLock<SslConnector> = OnceLock::new();
    let slot = match profile {
        Profile::Inference => &INFERENCE,
        Profile::Control => &CONTROL,
    };
    slot.get_or_init(|| build_connector(profile).expect("Claude TLS 上下文初始化失败"))
}

fn build_connector(profile: Profile) -> Result<SslConnector, boring2::error::ErrorStack> {
    let mut builder = SslConnector::builder(SslMethod::tls_client())?;
    builder.set_cipher_list(CIPHERS)?;
    builder.set_curves(CURVES)?;
    builder.set_sigalgs_list(SIGALGS)?;
    builder.set_grease_enabled(false);
    builder.set_permute_extensions(false);
    builder.set_min_proto_version(Some(SslVersion::TLS1_2))?;
    builder.set_max_proto_version(Some(SslVersion::TLS1_3))?;
    builder.set_key_shares_limit(1);
    if matches!(profile, Profile::Inference) {
        builder.set_alpn_protos(b"\x08http/1.1")?;
        builder.enable_ocsp_stapling();
        builder.enable_signed_cert_timestamps();
    }
    install_roots(&mut builder);
    Ok(builder.build())
}

/// 用系统信任库，公司的解密代理证书也认。装不上就留 BoringSSL 的默认路径。
fn install_roots(builder: &mut boring2::ssl::SslConnectorBuilder) {
    let loaded = rustls_native_certs::load_native_certs();
    if loaded.certs.is_empty() {
        return;
    }
    let Ok(mut store) = X509StoreBuilder::new() else {
        return;
    };
    let mut added = 0;
    for der in &loaded.certs {
        if let Ok(cert) = X509::from_der(der.as_ref()) {
            if store.add_cert(cert).is_ok() {
                added += 1;
            }
        }
    }
    if added > 0 {
        builder.set_cert_store(store.build());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_anthropic_itself_uses_this_handshake() {
        assert!(is_anthropic_api("https://api.anthropic.com"));
        assert!(is_anthropic_api("https://api.anthropic.com/v1/messages"));
        assert!(is_anthropic_api("https://API.ANTHROPIC.COM:443"));
        assert!(!is_anthropic_api("https://api.anthropic.com.evil.test"));
        assert!(!is_anthropic_api("https://platform.claude.com"));
        assert!(!is_anthropic_api("https://relay.example/anthropic"));
    }

    /// 抓本机 ClientHello，核对密码套件和曲线。不连外网。
    #[tokio::test]
    async fn inference_hello_matches_the_node_cipher_list() {
        let hello = capture(Profile::Inference).await;
        assert_eq!(
            hello.ciphers,
            vec![
                0x1301, 0x1302, 0x1303, 0xc02b, 0xc02f, 0xc02c, 0xc030, 0xcca9, 0xcca8, 0xc009,
                0xc013, 0xc00a, 0xc014, 0x009c, 0x009d, 0x002f, 0x0035,
            ],
            "ciphers {:04x?}\nextensions {:04x?}",
            hello.ciphers,
            hello.extensions
        );
        assert_eq!(hello.groups, vec![0x001d, 0x0017, 0x0018], "{hello:?}");
        assert_eq!(hello.alpn, vec!["http/1.1".to_string()], "{hello:?}");
        assert!(
            hello.extensions.iter().all(|id| !is_grease(*id)),
            "grease in {hello:?}"
        );
        let mut extensions = hello.extensions.clone();
        extensions.retain(|id| *id != 21);
        assert_eq!(
            extensions,
            vec![0, 23, 65281, 10, 11, 35, 16, 5, 13, 18, 51, 45, 43],
            "{hello:?}"
        );
    }

    #[tokio::test]
    async fn control_hello_omits_alpn_ocsp_and_sct() {
        let hello = capture(Profile::Control).await;
        let mut extensions = hello.extensions.clone();
        extensions.retain(|id| *id != 21);
        assert_eq!(
            extensions,
            vec![0, 23, 65281, 10, 11, 35, 13, 51, 45, 43],
            "{hello:?}"
        );
        assert!(hello.alpn.is_empty(), "{hello:?}");
        assert!(!hello.extensions.contains(&16), "{hello:?}");
        assert!(hello.extensions.iter().all(|id| !is_grease(*id)));
    }

    #[derive(Debug)]
    struct Hello {
        ciphers: Vec<u16>,
        extensions: Vec<u16>,
        groups: Vec<u16>,
        alpn: Vec<String>,
    }

    fn is_grease(id: u16) -> bool {
        id & 0x0f0f == 0x0a0a
    }

    async fn capture(profile: Profile) -> Hello {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::io::AsyncReadExt::read(&mut sock, &mut buf),
            )
            .await
            .expect("client hello timed out")
            .unwrap();
            buf.truncate(n);
            buf
        });
        let pending = tokio::spawn(async move {
            let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let mut conf = connector(profile).configure().unwrap();
            conf.set_enable_ech_grease(false);
            let _ = tokio_boring2::connect(conf, "api.anthropic.com", tcp).await;
        });
        let buf = accept.await.unwrap();
        pending.abort();
        parse_hello(&buf)
            .unwrap_or_else(|err| panic!("{err}; bytes {:02x?}", &buf[..buf.len().min(64)]))
    }

    fn parse_hello(buf: &[u8]) -> Result<Hello, String> {
        if buf.len() < 5 || buf[0] != 0x16 {
            return Err(format!("not a handshake record ({})", buf.len()));
        }
        let rec_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
        let body = buf
            .get(5..5 + rec_len)
            .ok_or_else(|| "truncated record".to_string())?;
        if body.first() != Some(&0x01) {
            return Err("not a ClientHello".into());
        }
        let mut i = 4 + 2 + 32;
        if i >= body.len() {
            return Err("truncated after random".into());
        }
        let sid_len = body[i] as usize;
        i += 1 + sid_len;
        let cs_len = u16_at(body, i)? as usize;
        i += 2;
        let mut ciphers = Vec::new();
        let cs = body
            .get(i..i + cs_len)
            .ok_or_else(|| "truncated ciphers".to_string())?;
        for pair in cs.chunks(2) {
            if pair.len() == 2 {
                ciphers.push(u16::from_be_bytes([pair[0], pair[1]]));
            }
        }
        i += cs_len;
        let comp_len = *body.get(i).ok_or("truncated compression")? as usize;
        i += 1 + comp_len;
        let ext_len = u16_at(body, i)? as usize;
        i += 2;
        let ext_end = i + ext_len;
        if ext_end > body.len() {
            return Err("truncated extensions".into());
        }
        let mut extensions = Vec::new();
        let mut groups = Vec::new();
        let mut alpn = Vec::new();
        while i + 4 <= ext_end {
            let typ = u16_at(body, i)?;
            let len = u16_at(body, i + 2)? as usize;
            i += 4;
            let data = body
                .get(i..i + len)
                .ok_or_else(|| format!("truncated extension {typ}"))?;
            if typ == 10 {
                groups = parse_u16_list(data);
            }
            if typ == 16 {
                alpn = parse_alpn(data);
            }
            extensions.push(typ);
            i += len;
        }
        Ok(Hello {
            ciphers,
            extensions,
            groups,
            alpn,
        })
    }

    fn u16_at(buf: &[u8], i: usize) -> Result<u16, String> {
        let pair = buf.get(i..i + 2).ok_or_else(|| "short u16".to_string())?;
        Ok(u16::from_be_bytes([pair[0], pair[1]]))
    }

    fn parse_u16_list(data: &[u8]) -> Vec<u16> {
        if data.len() < 2 {
            return Vec::new();
        }
        let n = u16::from_be_bytes([data[0], data[1]]) as usize;
        data.get(2..2 + n)
            .unwrap_or(&[])
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect()
    }

    fn parse_alpn(data: &[u8]) -> Vec<String> {
        if data.len() < 2 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut p = 2;
        let end = 2 + u16::from_be_bytes([data[0], data[1]]) as usize;
        while p < end && p < data.len() {
            let n = data[p] as usize;
            p += 1;
            if p + n > data.len() {
                break;
            }
            out.push(String::from_utf8_lossy(&data[p..p + n]).into_owned());
            p += n;
        }
        out
    }
}
