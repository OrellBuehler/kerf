//! `import_url`'s download: an http(s) URL into the workspace, for a headless server started with
//! `--workspace DIR --allow-url-import`.
//!
//! A server that fetches what a client names can be pointed at what only the server reaches (its
//! loopback, the private network behind it, a cloud metadata endpoint). So every hop is resolved
//! here, refused unless **every** address the name gives is public ([`is_public`]), and connected to
//! by those vetted addresses alone ([`Pinned`]: ureq never looks the name up again, so a second DNS
//! answer cannot swap a private address in). Redirects are followed by hand, each hop checked the
//! same way, at most [`MAX_REDIRECTS`]. No proxy is used, whatever the environment says: the check
//! is about the address actually connected to.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ureq::http::Uri;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// Redirects followed before giving up.
pub(super) const MAX_REDIRECTS: usize = 5;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
/// The whole body's budget. The reader thread cannot be interrupted, so a stalled read lingers
/// until this at the latest; [`MAX_READERS`] bounds how many can.
const BODY_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
/// Downloads whose reader thread is alive (one given up on still counts until its read returns).
const MAX_READERS: usize = 4;
static READERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// How long the body may send nothing before the download is given up.
pub(super) const STALL: Duration = Duration::from_secs(60);
/// How often a blocked read looks at the cancel.
const POLL: Duration = Duration::from_millis(200);

/// Whether a connection to `ip` reaches the public internet: not loopback, private, link-local,
/// unique-local, CGNAT, multicast, unspecified, broadcast, documentation, benchmarking or reserved
/// space, nor an IPv6 address that embeds such an IPv4 one (mapped, compatible, NAT64, 6to4).
pub(super) fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_public(v4),
        IpAddr::V6(v6) => v6_public(v6),
    }
}

fn v4_public(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let metadata = [[168, 63, 129, 16], [100, 100, 100, 200]].contains(&ip.octets()); // Azure WireServer, Alibaba
    let refused = metadata
        || a == 0 // "this network", 0.0.0.0
        || a == 10
        || a == 127
        || (a == 100 && (64..128).contains(&b)) // CGNAT 100.64/10 (Alibaba's metadata is 100.100.100.200)
        || (a == 169 && b == 254) // link-local, the cloud metadata endpoint 169.254.169.254
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments (Oracle's metadata 192.0.0.192)
        || (a == 192 && b == 0 && c == 2) // TEST-NET-1
        || (a == 192 && b == 88 && c == 99) // 6to4 relay anycast
        || (a == 192 && b == 168)
        || (a == 198 && (18..20).contains(&b)) // benchmarking 198.18/15
        || (a == 198 && b == 51 && c == 100) // TEST-NET-2
        || (a == 203 && b == 0 && c == 113) // TEST-NET-3
        || a >= 224; // multicast 224/4, reserved 240/4, broadcast
    !refused
}

fn v6_public(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let embedded = |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
    // ::ffff:a.b.c.d (mapped) is the IPv4 address; ::/96 (IPv4-compatible, deprecated; :: and ::1
    // among it) is refused.
    if s[..6] == [0, 0, 0, 0, 0, 0xffff] {
        return v4_public(embedded(s[6], s[7]));
    }
    if s[..6] == [0; 6] {
        return false;
    }
    // NAT64 64:ff9b::/96 reaches the IPv4 address it carries; 64:ff9b:1::/48 is local use.
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return v4_public(embedded(s[6], s[7]));
    }
    // 6to4 2002::/16 carries an IPv4 address in its next 32 bits.
    if s[0] == 0x2002 {
        return v4_public(embedded(s[1], s[2]));
    }
    // Only global unicast 2000::/3, without the special blocks inside it: 2001::/23 (IETF protocol
    // assignments, Teredo among them), 2001:db8::/32 and 3fff::/20 (documentation).
    let global = (s[0] & 0xe000) == 0x2000;
    let special = (s[0] == 0x2001 && s[1] < 0x0200) || (s[0] == 0x2001 && s[1] == 0x0db8) || (s[0] == 0x3fff && s[1] < 0x1000);
    global && !special
}

/// The URL a client named, if it is one this server fetches: `http` or `https` with a host and no
/// credentials.
pub(super) fn check_url(url: &str) -> Result<Uri, String> {
    let scheme = url.trim().split_once(':').map(|(s, _)| s.to_ascii_lowercase());
    if let Some(scheme) = scheme.filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))) {
        if scheme != "http" && scheme != "https" {
            return Err(format!("{url:?}: only http and https URLs are fetched, not {scheme}:"));
        }
    }
    let uri: Uri = url.trim().parse().map_err(|e| format!("{url:?} is not a URL: {e}"))?;
    match uri.scheme_str().map(str::to_ascii_lowercase).as_deref() {
        Some("http" | "https") => {}
        Some(other) => return Err(format!("{url:?}: only http and https URLs are fetched, not {other}:")),
        None => return Err(format!("{url:?} is not an absolute http(s) URL")),
    }
    let authority = uri.authority().ok_or_else(|| format!("{url:?} names no host"))?;
    if authority.as_str().contains('@') {
        return Err(format!(
            "{url:?} carries credentials; they are not sent: give a URL without user:password@"
        ));
    }
    if authority.host().is_empty() {
        return Err(format!("{url:?} names no host"));
    }
    Ok(uri)
}

/// `uri`'s host (without brackets) and port.
fn host_port(uri: &Uri) -> (String, u16) {
    let https = uri.scheme_str().is_some_and(|s| s.eq_ignore_ascii_case("https"));
    let authority = uri.authority().expect("checked");
    let host = authority.host().trim_start_matches('[').trim_end_matches(']').to_string();
    (host, authority.port_u16().unwrap_or(if https { 443 } else { 80 }))
}

/// Every address `host` resolves to, within [`RESOLVE_TIMEOUT`].
fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let name = host.to_string();
    std::thread::spawn(move || {
        let _ = tx.send((name.as_str(), port).to_socket_addrs().map(Iterator::collect::<Vec<_>>));
    });
    match rx.recv_timeout(RESOLVE_TIMEOUT) {
        Ok(Ok(addrs)) if !addrs.is_empty() => Ok(addrs),
        _ => Err(not_public(host)),
    }
}

/// The one answer for a host that does not resolve and for one that is not public: the client
/// learns nothing about what the server's resolver sees.
fn not_public(host: &str) -> String {
    format!(
        "{host} is not a public address this server fetches from (it does not resolve, or it resolves to a \
         loopback, private, link-local, metadata, reserved or this server's own address)"
    )
}

/// `addrs` (what `host` resolved to) when `permit` takes every one of them: a name that answers
/// with a private address among public ones is refused whole, never connected to by the public one
/// alone.
fn vet(host: &str, addrs: Vec<SocketAddr>, limits: &Limits) -> Result<Vec<SocketAddr>, String> {
    let own = |ip: IpAddr| limits.own.iter().any(|o| o.to_canonical() == ip.to_canonical());
    if addrs.iter().any(|a| !(limits.permit)(a.ip()) || own(a.ip())) {
        return Err(not_public(host));
    }
    Ok(addrs)
}

/// The addresses of this machine's own interfaces (a public one too: the server must not fetch from
/// itself). Empty where the system does not list them (Windows).
pub(super) fn own_addresses() -> Vec<IpAddr> {
    let mut out = Vec::new();
    #[cfg(unix)]
    // SAFETY: `getifaddrs` hands back a list this reads, following `ifa_next` to the null end, and
    // frees with `freeifaddrs`; an address is read as `sockaddr_in` / `sockaddr_in6` only when its
    // family says it is one.
    unsafe {
        let mut head = std::mem::MaybeUninit::<*mut libc::ifaddrs>::uninit();
        if libc::getifaddrs(head.as_mut_ptr()) != 0 {
            return out;
        }
        let head = head.assume_init();
        let mut next = std::ptr::NonNull::new(head);
        while let Some(entry) = next {
            let ifa = entry.as_ref();
            if let Some(addr) = std::ptr::NonNull::new(ifa.ifa_addr) {
                match i32::from(addr.as_ref().sa_family) {
                    libc::AF_INET => {
                        let sin = addr.cast::<libc::sockaddr_in>().as_ref();
                        out.push(IpAddr::V4(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr))));
                    }
                    libc::AF_INET6 => {
                        let sin6 = addr.cast::<libc::sockaddr_in6>().as_ref();
                        out.push(IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr)));
                    }
                    _ => {}
                }
            }
            next = std::ptr::NonNull::new(ifa.ifa_next);
        }
        if let Some(head) = std::ptr::NonNull::new(head) {
            libc::freeifaddrs(head.as_ptr());
        }
    }
    out
}

/// A resolver that answers the one host it was made for with the addresses already vetted, and
/// nothing else: ureq connects to those, and never asks DNS again.
#[derive(Debug)]
struct Pinned {
    host: String,
    addrs: Vec<SocketAddr>,
}

impl Resolver for Pinned {
    fn resolve(
        &self,
        uri: &Uri,
        _config: &ureq::config::Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let host = uri.host().unwrap_or_default().trim_start_matches('[').trim_end_matches(']');
        if !host.eq_ignore_ascii_case(&self.host) {
            return Err(ureq::Error::HostNotFound);
        }
        let mut out = self.empty();
        for addr in self.addrs.iter().take(16) {
            out.push(*addr);
        }
        Ok(out)
    }
}

/// `location` (a redirect's `Location`) read against the URL that sent it.
pub(super) fn join(base: &Uri, location: &str) -> Result<String, String> {
    let location = location.trim();
    if location.contains("://") {
        return Ok(location.to_string());
    }
    let scheme = base.scheme_str().unwrap_or("http");
    if let Some(rest) = location.strip_prefix("//") {
        return Ok(format!("{scheme}://{rest}"));
    }
    let authority = base.authority().map(|a| a.as_str()).unwrap_or_default();
    if location.starts_with('/') {
        return Ok(format!("{scheme}://{authority}{location}"));
    }
    if location.is_empty() {
        return Err("a redirect with an empty Location".to_string());
    }
    let path = base.path();
    let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    Ok(format!("{scheme}://{authority}{dir}/{location}"))
}

/// A file name for what `url` points at: its last path segment, decoded, with anything but letters,
/// digits, `.`, `-` and `_` made `_`; `download` when nothing is left.
pub(super) fn file_name(url: &Uri) -> String {
    let last = url.path().rsplit('/').next().unwrap_or_default();
    let decoded = super::files::percent_decode(last).unwrap_or_else(|_| last.to_string());
    let mut name: String = decoded
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    name = name.trim_start_matches('.').to_string();
    if name.len() > 120 {
        let ext = Path::new(&name)
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| e.len() <= 8)
            .map(str::to_string);
        name.truncate(110);
        if let Some(ext) = ext {
            name = format!("{name}.{ext}");
        }
    }
    if name.trim_matches('_').is_empty() {
        "download".to_string()
    } else {
        name
    }
}

/// What a download needs besides the URL.
pub(super) struct Limits {
    /// The most bytes written; a body that announces or sends more is refused.
    pub max_bytes: u64,
    /// The silence tolerated before the download is given up.
    pub stall: Duration,
    /// Which addresses may be connected to: [`is_public`] (a test passes its own, to reach a local server).
    pub permit: fn(IpAddr) -> bool,
    /// This machine's own addresses ([`own_addresses`]), refused whatever `permit` says.
    pub own: Vec<IpAddr>,
}

/// Download `url` into `dest` (inside the workspace, not there yet): written to a `.part` beside it,
/// renamed into place only once whole, refused if `dest` appeared meanwhile. `progress` hears the
/// bytes written and the size announced; `cancel` is looked at every [`POLL`].
/// `shown` is `dest` as the client sees it (relative to the workspace), for the messages.
pub(super) fn download(
    url: &str,
    dest: &Path,
    shown: &str,
    limits: &Limits,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<u64, String> {
    let reader = super::files::Slot::take(&READERS, MAX_READERS)
        .ok_or_else(|| format!("{MAX_READERS} URL downloads are already under way; try again when one is done"))?;
    let response = follow(url, limits)?;
    let encoding = response
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty() && v != "identity");
    // ureq decodes gzip itself (a crate feature the model downloads share) and drops the header:
    // that body is counted against the cap as it is decoded. Any other encoding is refused.
    if let Some(encoding) = encoding {
        return Err(format!(
            "{url} is sent {encoding}-encoded; only plain (identity) bodies are taken"
        ));
    }
    let total = response.body().content_length();
    if let Some(total) = total.filter(|t| *t > limits.max_bytes) {
        return Err(format!(
            "{url} is {total} bytes, more than this server takes ({} bytes, --max-upload-gb)",
            limits.max_bytes
        ));
    }
    let dir = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    super::files::check_room(&dir, total.unwrap_or(0), shown).map_err(|e| e.to_string())?;
    let part = PartFile::create(dest, shown)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&part.0)
        .map_err(|e| write_error(shown, &e))?;
    // The body is read on a thread of its own so a cancel and a stall are noticed while a read is
    // blocked; the bounded channel throttles the read to the disk.
    let mut body = response.into_body().into_reader();
    let (chunks, received) = std::sync::mpsc::sync_channel::<std::io::Result<Vec<u8>>>(4);
    std::thread::spawn(move || {
        // The slot is the thread's: it is given back when the read returns, not when we give up.
        let _reader = reader;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            match body.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => {
                    if chunks.send(Ok(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = chunks.send(Err(e));
                    return;
                }
            }
        }
    });
    let mut written = 0u64;
    let mut reported = 0u64;
    let mut room_checked = 0u64;
    let mut heard = Instant::now();
    progress(0, total);
    loop {
        if cancel() {
            return Err("cancelled".to_string());
        }
        let chunk = match received.recv_timeout(POLL.min(limits.stall)) {
            Ok(Ok(chunk)) => chunk,
            Ok(Err(e)) => return Err(format!("the download of {url} failed: {e}")),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if heard.elapsed() >= limits.stall {
                    return Err(format!("{url} sent nothing for {} s", limits.stall.as_secs().max(1)));
                }
                continue;
            }
        };
        heard = Instant::now();
        written += chunk.len() as u64;
        if written > limits.max_bytes {
            return Err(format!(
                "{url} sent more than this server takes ({} bytes, --max-upload-gb)",
                limits.max_bytes
            ));
        }
        file.write_all(&chunk).map_err(|e| write_error(shown, &e))?;
        if written - room_checked >= super::files::ROOM_CHECK_EVERY {
            room_checked = written;
            super::files::check_room(&dir, 0, shown).map_err(|e| e.to_string())?;
        }
        if written - reported >= 1024 * 1024 {
            reported = written;
            progress(written, total);
        }
    }
    if let Some(total) = total.filter(|t| *t != written) {
        return Err(format!("the download of {url} is incomplete ({written} of {total} bytes)"));
    }
    file.sync_all().map_err(|e| write_error(shown, &e))?;
    drop(file);
    part.commit(dest, shown)?;
    progress(written, Some(written));
    Ok(written)
}

fn write_error(shown: &str, e: &std::io::Error) -> String {
    if super::files::disk_full(e) {
        format!("the disk is full: {shown} was not written")
    } else {
        format!("could not write {shown}: {}", e.kind())
    }
}

/// The response that ends `url`'s redirect chain, each hop's URL checked and its host resolved and
/// vetted before it is connected to.
fn follow(url: &str, limits: &Limits) -> Result<ureq::http::Response<ureq::Body>, String> {
    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let uri = check_url(&url)?;
        let (host, port) = host_port(&uri);
        let addrs = vet(&host, resolve(&host, port)?, limits)?;
        let config = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .accept_encoding("identity")
            .user_agent(concat!("kerf/", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_send_request(Some(RESPONSE_TIMEOUT))
            .timeout_recv_response(Some(RESPONSE_TIMEOUT))
            .timeout_recv_body(Some(BODY_TIMEOUT))
            .build();
        let agent = ureq::Agent::with_parts(config, DefaultConnector::new(), Pinned { host, addrs });
        let response = agent.get(&uri).call().map_err(|e| format!("could not fetch {url}: {e}"))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|l| l.to_str().ok())
                .ok_or_else(|| format!("{url} redirects (HTTP {}) without a Location", status.as_u16()))?;
            url = join(&uri, location)?;
            continue;
        }
        if !status.is_success() {
            return Err(format!("{url} answered HTTP {}", status.as_u16()));
        }
        return Ok(response);
    }
    Err(format!("{url} redirects more than {MAX_REDIRECTS} times"))
}

/// The `.part` a download writes, removed unless it was renamed into place.
struct PartFile(PathBuf);

impl PartFile {
    fn create(dest: &Path, shown: &str) -> Result<Self, String> {
        let path = super::files::part_path(dest);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| write_error(shown, &e))?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| write_error(shown, &e))?;
        Ok(Self(path))
    }

    /// Put the file in place at `dest`, refused when something is there by now.
    fn commit(self, dest: &Path, shown: &str) -> Result<(), String> {
        super::files::place(&self.0, dest, shown, false).map_err(|e| e.to_string())
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(text: &str) -> bool {
        is_public(text.parse().unwrap())
    }

    #[test]
    fn private_and_special_ipv4_is_refused() {
        for refused in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "10.255.255.255",
            "100.64.0.0",
            "100.100.100.200",
            "100.127.255.255",
            "168.63.129.16",
            "127.0.0.1",
            "127.255.255.254",
            "169.254.169.254",
            "169.254.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.192",
            "192.0.2.10",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!v4(refused), "{refused}");
        }
    }

    #[test]
    fn public_ipv4_and_the_edges_of_the_ranges_pass() {
        for public in [
            "1.1.1.1",
            "8.8.8.8",
            "9.255.255.255",
            "11.0.0.0",
            "100.63.255.255",
            "100.128.0.0",
            "126.255.255.255",
            "128.0.0.1",
            "168.63.129.17",
            "169.253.255.255",
            "169.255.0.0",
            "172.15.255.255",
            "172.32.0.0",
            "192.0.1.1",
            "192.0.3.1",
            "192.167.255.255",
            "192.169.0.0",
            "198.17.255.255",
            "198.20.0.0",
            "203.0.112.1",
            "223.255.255.255",
        ] {
            assert!(v4(public), "{public}");
        }
    }

    #[test]
    fn ipv6_is_global_unicast_only_and_embedded_ipv4_is_judged_as_ipv4() {
        for refused in [
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:0.0.0.0",
            "::127.0.0.1",
            "::10.1.2.3",
            "::8.8.8.8",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::1",
            "100::1",
            "2001::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2001:db8::1",
            "2001:1ff:ffff::1",
            "2002:7f00:1::1",
            "2002:c0a8:101::1",
            "3fff::1",
            "3fff:fff::1",
            "fc00::1",
            "fd00::1",
            "fd00:ec2::254",
            "fe80::1",
            "febf::1",
            "fec0::1",
            "ff02::1",
            "ff0e::1",
            "4000::1",
            "5f00::1",
        ] {
            assert!(!v4(refused), "{refused}");
        }
        for public in [
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
            "2001:200::1",
            "2a00:1450:4001::1",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::1",
            "3ffe::1",
        ] {
            assert!(v4(public), "{public}");
        }
    }

    #[test]
    fn only_http_and_https_urls_without_credentials_are_fetched() {
        assert!(check_url("file:///etc/passwd").unwrap_err().contains("only http and https"));
        assert!(check_url("https://example.com/a.mp4").is_ok());
        assert!(check_url("HTTP://example.com:8080/a").is_ok());
        assert!(check_url("http://[2606:4700::1]/a").is_ok());
        for refused in [
            "file:///etc/passwd",
            "ftp://example.com/a",
            "gopher://example.com",
            "/etc/passwd",
            "example.com/a.mp4",
            "http://user:pw@example.com/a",
            "http://@example.com/a",
            "not a url",
        ] {
            assert!(check_url(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_name_with_one_private_address_is_refused_whole() {
        let public = Limits {
            permit: is_public,
            ..limits(1)
        };
        let addrs = vec!["8.8.8.8:80".parse().unwrap(), "10.0.0.1:80".parse().unwrap()];
        let why = vet("mixed.example", addrs, &public).unwrap_err();
        assert!(!why.contains("10.0.0.1") && why.contains("not a public address"), "{why}");
        assert!(vet("ok.example", vec!["8.8.8.8:80".parse().unwrap()], &public).is_ok());
        // Every spelling of an address resolves before it is judged.
        let literal = resolve("127.0.0.1", 80).unwrap();
        assert!(vet("127.0.0.1", literal, &public).is_err());
        // A name that does not resolve reads as one that is not public.
        assert_eq!(
            resolve("no-such-host.invalid", 80).unwrap_err(),
            not_public("no-such-host.invalid")
        );
        // This machine's own addresses are refused, a public one or its mapped spelling too.
        let own = Limits {
            own: vec!["8.8.4.4".parse().unwrap()],
            ..public
        };
        assert!(vet("self.example", vec!["8.8.4.4:80".parse().unwrap()], &own).is_err());
        assert!(vet("self.example", vec!["[::ffff:8.8.4.4]:80".parse().unwrap()], &own).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_machine_lists_its_own_loopback() {
        assert!(own_addresses().iter().any(|a| a.is_loopback()), "{:?}", own_addresses());
    }

    #[test]
    fn redirects_are_read_against_the_url_that_sent_them() {
        let base: Uri = "https://a.example/media/clip.mp4?x=1".parse().unwrap();
        assert_eq!(join(&base, "https://b.example/c").unwrap(), "https://b.example/c");
        assert_eq!(join(&base, "//cdn.example/c").unwrap(), "https://cdn.example/c");
        assert_eq!(join(&base, "/other").unwrap(), "https://a.example/other");
        assert_eq!(join(&base, "next.mp4").unwrap(), "https://a.example/media/next.mp4");
        assert!(join(&base, "").is_err());
    }

    #[test]
    fn a_file_name_is_the_last_segment_made_safe() {
        let name = |url: &str| file_name(&url.parse().unwrap());
        assert_eq!(name("https://a.example/media/My%20Clip.mp4?x=1"), "My_Clip.mp4");
        assert_eq!(name("https://a.example/"), "download");
        assert_eq!(name("https://a.example/..%2F..%2Fetc%2Fpasswd"), "_.._etc_passwd");
        assert_eq!(name("https://a.example/.hidden"), "hidden");
        assert_eq!(name("https://a.example/%E2%9C%93"), "download");
        assert!(name(&format!("https://a.example/{}.mp4", "x".repeat(300))).ends_with(".mp4"));
        assert!(name(&format!("https://a.example/{}.mp4", "x".repeat(300))).len() <= 120);
    }

    /// A server on loopback that answers each connection with the next of `answers` (raw HTTP).
    fn serve(answers: Vec<Vec<u8>>) -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for answer in answers {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let mut seen = Vec::new();
                let mut buf = [0u8; 1024];
                while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => seen.extend_from_slice(&buf[..n]),
                    }
                }
                let _ = stream.write_all(&answer);
            }
        });
        addr
    }

    fn ok(body: &[u8]) -> Vec<u8> {
        let mut answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        answer.extend_from_slice(body);
        answer
    }

    fn redirect(to: &str) -> Vec<u8> {
        format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
    }

    fn loopback(ip: IpAddr) -> bool {
        ip.is_loopback()
    }

    fn limits(max_bytes: u64) -> Limits {
        Limits {
            max_bytes,
            stall: Duration::from_secs(5),
            permit: loopback,
            own: Vec::new(),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-fetch-{name}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn download_to(
        url: &str,
        dest: &Path,
        limits: &Limits,
        cancel: &dyn Fn() -> bool,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<u64, String> {
        download(url, dest, "the file", limits, cancel, progress)
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .collect()
    }

    #[test]
    fn a_download_follows_a_redirect_and_lands_whole() {
        let target = serve(vec![ok(b"media bytes")]);
        let first = serve(vec![redirect(&format!("http://{target}/b/clip.mp4"))]);
        let dir = scratch("ok");
        let dest = dir.join("sub").join("clip.mp4");
        let mut heard = Vec::new();
        let size = download_to(
            &format!("http://{first}/a"),
            &dest,
            &limits(1 << 20),
            &|| false,
            &mut |w, t| heard.push((w, t)),
        )
        .unwrap();
        assert_eq!(size, 11);
        assert_eq!(std::fs::read(&dest).unwrap(), b"media bytes");
        assert_eq!(heard.last(), Some(&(11, Some(11))));
        assert_eq!(
            leftovers(dest.parent().unwrap()),
            vec!["clip.mp4".to_string()],
            "no .part left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_public_address_rule_holds_on_every_hop() {
        let dir = scratch("hop");
        let target = serve(vec![ok(b"secret")]);
        let first = serve(vec![redirect(&format!("http://{target}/x"))]);
        let public_only = Limits {
            permit: is_public,
            ..limits(1 << 20)
        };
        let why = download_to(
            &format!("http://{first}/a"),
            &dir.join("a"),
            &public_only,
            &|| false,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(why.contains("not a public address"), "{why}");
        // A redirect to another scheme is refused like a first URL.
        let to_file = serve(vec![redirect("file:///etc/passwd")]);
        let why = download_to(
            &format!("http://{to_file}/a"),
            &dir.join("b"),
            &limits(1 << 20),
            &|| false,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(why.contains("only http and https"), "{why}");
        assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn too_many_redirects_too_big_a_body_and_an_existing_file_are_refused() {
        let dir = scratch("refused");
        let addr = serve(vec![]);
        let looping: Vec<Vec<u8>> = (0..=MAX_REDIRECTS).map(|_| redirect("/again")).collect();
        let looped = serve(looping);
        let why = download_to(
            &format!("http://{looped}/a"),
            &dir.join("a"),
            &limits(1 << 20),
            &|| false,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(why.contains("redirects more than"), "{why}");
        let _ = addr;

        let big = serve(vec![ok(&[7u8; 64])]);
        let why = download_to(
            &format!("http://{big}/a"),
            &dir.join("big"),
            &limits(16),
            &|| false,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(why.contains("more than this server takes"), "{why}");
        // A body with no length that runs past the cap is cut where it passes it.
        let mut unbounded = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        unbounded.extend_from_slice(&[7u8; 64]);
        let endless = serve(vec![unbounded]);
        let why = download_to(
            &format!("http://{endless}/a"),
            &dir.join("big"),
            &limits(16),
            &|| false,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(why.contains("sent more than"), "{why}");

        let mut gzipped = b"HTTP/1.1 200 OK\r\nContent-Encoding: br\r\nContent-Length: 4\r\nConnection: close\r\n\r\n".to_vec();
        gzipped.extend_from_slice(b"abcd");
        let encoded = serve(vec![gzipped]);
        let why = download_to(
            &format!("http://{encoded}/a"),
            &dir.join("gz"),
            &limits(1 << 20),
            &|| false,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(why.contains("br-encoded"), "{why}");

        std::fs::write(dir.join("there"), b"mine").unwrap();
        let again = serve(vec![ok(b"theirs")]);
        let why = download_to(
            &format!("http://{again}/a"),
            &dir.join("there"),
            &limits(1 << 20),
            &|| false,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(why.contains("exists"), "{why}");
        assert_eq!(std::fs::read(dir.join("there")).unwrap(), b"mine");
        assert_eq!(leftovers(&dir), vec!["there".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancel_stops_a_body_that_never_ends() {
        let dir = scratch("cancel");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\nstart");
            std::thread::sleep(Duration::from_secs(10));
        });
        let started = Instant::now();
        let why = download_to(
            &format!("http://{addr}/a"),
            &dir.join("a"),
            &limits(1 << 20),
            &|| started.elapsed() > Duration::from_millis(300),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert_eq!(why, "cancelled");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(leftovers(&dir).is_empty(), "the .part is removed: {:?}", leftovers(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
