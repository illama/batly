//! Outgoing HTTP(S) requests.
//!
//! On Windows this goes through WinHTTP, which is part of the system: TLS,
//! proxies and certificate checks come for free and the executable stays
//! small. Plain `http://` URLs are also handled by a tiny `std::net` client
//! on every platform (used by the tests and for LAN peers).

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub const USER_AGENT: &str = concat!("Batly/", env!("CARGO_PKG_VERSION"));

#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

pub fn get(url: &str, headers: &[(&str, &str)], timeout: Duration) -> io::Result<Reply> {
    request("GET", url, headers, None, timeout)
}

pub fn post_json(url: &str, json: &str, timeout: Duration) -> io::Result<Reply> {
    request("POST", url, &[("Content-Type", "application/json")], Some(json.as_bytes()), timeout)
}

/// Performs a request; redirects are followed (https only on Windows).
pub fn request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    timeout: Duration,
) -> io::Result<Reply> {
    let parsed = Url::parse(url).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad URL"))?;
    if !parsed.https {
        return plain_http(method, &parsed, headers, body, timeout);
    }
    #[cfg(windows)]
    {
        winhttp::request(method, url, headers, body, timeout)
    }
    #[cfg(not(windows))]
    {
        Err(io::Error::new(io::ErrorKind::Unsupported, "https needs Windows"))
    }
}

#[derive(Debug, PartialEq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, always starting with '/'.
    pub path: String,
}

impl Url {
    pub fn parse(url: &str) -> Option<Self> {
        let url = url.trim();
        let (https, rest) = match url.strip_prefix("https://") {
            Some(r) => (true, r),
            None => (false, url.strip_prefix("http://")?),
        };
        let rest = rest.split('#').next()?;
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) => (&rest[..i], rest[i..].to_string()),
            None => (rest, "/".to_string()),
        };
        let path = if path.starts_with('?') { format!("/{path}") } else { path };
        let authority = authority.rsplit('@').next()?;
        let default_port = if https { 443 } else { 80 };
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (h, rest) = v6.split_once(']')?;
            let port = match rest.strip_prefix(':') {
                Some(p) => p.parse().ok()?,
                None => default_port,
            };
            (h.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse().ok()?),
                None => (authority.to_string(), default_port),
            }
        };
        if host.is_empty() {
            return None;
        }
        Some(Self { https, host, port, path })
    }
}

/// Splits `url#token` (the peer syntax of the settings file).
pub fn split_token(url: &str) -> (&str, Option<&str>) {
    match url.split_once('#') {
        Some((u, t)) if !t.is_empty() => (u, Some(t)),
        Some((u, _)) => (u, None),
        None => (url, None),
    }
}

fn plain_http(method: &str, url: &Url, headers: &[(&str, &str)], body: Option<&[u8]>, timeout: Duration) -> io::Result<Reply> {
    let addr = (url.host.as_str(), url.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "host not found"))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let host = if url.host.contains(':') { format!("[{}]", url.host) } else { url.host.clone() };
    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {host}:{}\r\nUser-Agent: {USER_AGENT}\r\nAccept: */*\r\nConnection: close\r\n",
        url.path, url.port
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        head.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if let Some(b) = body {
        stream.write_all(b)?;
    }
    let mut raw = Vec::new();
    stream.take(16 * 1024 * 1024).read_to_end(&mut raw)?;
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> io::Result<Reply> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "bad HTTP response");
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(bad)?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?;
    let mut body = raw[split + 4..].to_vec();
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    if chunked {
        body = dechunk(&body).ok_or_else(bad)?;
    }
    Ok(Reply { status, body })
}

fn dechunk(mut data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = data.windows(2).position(|w| w == b"\r\n")?;
        let size_text = std::str::from_utf8(&data[..eol]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        data = &data[eol + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(data.get(..size)?);
        data = data.get(size + 2..)?;
    }
}

#[cfg(windows)]
mod winhttp {
    use super::{Reply, USER_AGENT};
    use crate::win::wide;
    use std::io;
    use std::ptr::{null, null_mut};
    use std::time::Duration;
    use windows_sys::Win32::Networking::WinHttp::*;

    struct Handle(*mut core::ffi::c_void);

    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: we own the handle.
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }

    fn check(h: *mut core::ffi::c_void) -> io::Result<Handle> {
        if h.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Handle(h))
        }
    }

    fn ok(b: i32) -> io::Result<()> {
        if b == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn request(
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
        timeout: Duration,
    ) -> io::Result<Reply> {
        let parsed = super::Url::parse(url).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad URL"))?;
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: every pointer passed below points to live, NUL-terminated
        // buffers; handles are closed by `Handle`'s destructor.
        unsafe {
            let session = check(WinHttpOpen(
                wide(USER_AGENT).as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                null(),
                null(),
                0,
            ))?;
            WinHttpSetTimeouts(session.0, ms, ms, ms, ms);
            let connect = check(WinHttpConnect(session.0, wide(&parsed.host).as_ptr(), parsed.port, 0))?;
            let flags = if parsed.https { WINHTTP_FLAG_SECURE } else { 0 };
            let req = check(WinHttpOpenRequest(
                connect.0,
                wide(method).as_ptr(),
                wide(&parsed.path).as_ptr(),
                null(),
                null(),
                null(),
                flags,
            ))?;
            let mut head = String::new();
            for (k, v) in headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            let head_w = wide(&head);
            let (body_ptr, body_len) = match body {
                Some(b) => (b.as_ptr().cast(), b.len() as u32),
                None => (null(), 0),
            };
            ok(WinHttpSendRequest(
                req.0,
                if head.is_empty() { null() } else { head_w.as_ptr() },
                if head.is_empty() { 0 } else { u32::MAX },
                body_ptr,
                body_len,
                body_len,
                0,
            ))?;
            ok(WinHttpReceiveResponse(req.0, null_mut()))?;
            let mut status: u32 = 0;
            let mut size = 4u32;
            ok(WinHttpQueryHeaders(
                req.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&mut status as *mut u32).cast(),
                &mut size,
                null_mut(),
            ))?;
            let mut out = Vec::new();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let mut read = 0u32;
                ok(WinHttpReadData(req.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut read))?;
                if read == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..read as usize]);
                if out.len() > 64 * 1024 * 1024 {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "response too large"));
                }
            }
            Ok(Reply { status: status as u16, body: out })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn urls() {
        let u = Url::parse("https://api.github.com/repos/x/y?a=1").unwrap();
        assert_eq!((u.https, u.host.as_str(), u.port, u.path.as_str()), (true, "api.github.com", 443, "/repos/x/y?a=1"));
        let u = Url::parse("http://pc-salon:12321#tok").unwrap();
        assert_eq!((u.https, u.host.as_str(), u.port, u.path.as_str()), (false, "pc-salon", 12321, "/"));
        let u = Url::parse("http://[::1]:8080/api").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("::1", 8080));
        assert_eq!(Url::parse("http://user:pw@h?x").unwrap().path, "/?x");
        assert!(Url::parse("ftp://x").is_none() && Url::parse("http://").is_none());
        assert_eq!(split_token("http://a:1#t0k"), ("http://a:1", Some("t0k")));
        assert_eq!(split_token("http://a:1"), ("http://a:1", None));
    }

    #[test]
    fn chunked_and_plain_responses() {
        let r = parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5;x\r\npedia\r\n0\r\n\r\n").unwrap();
        assert_eq!((r.status, r.text().as_str()), (200, "Wikipedia"));
        let r = parse_response(b"HTTP/1.0 404 Not Found\r\n\r\nnope").unwrap();
        assert_eq!((r.status, r.ok()), (404, false));
        assert!(parse_response(b"garbage").is_err());
    }

    #[test]
    fn talks_plain_http() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let mut req = String::new();
            while !req.ends_with('}') {
                let n = s.read(&mut buf).unwrap();
                assert!(n > 0, "{req}");
                req.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
            assert!(req.starts_with("POST /hook HTTP/1.1\r\n"), "{req}");
            assert!(req.contains("Content-Type: application/json") && req.ends_with("{\"a\":1}"));
            s.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n").unwrap();
        });
        let r = post_json(&format!("http://127.0.0.1:{port}/hook"), "{\"a\":1}", Duration::from_secs(2)).unwrap();
        assert_eq!(r.status, 204);
    }
}
