use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use encoding_rs::Encoding;
use reqwest::header::{CONTENT_TYPE, COOKIE, HeaderMap, LOCATION, SET_COOKIE, USER_AGENT};
use reqwest::{Client, StatusCode};

use crate::block::Blocker;

const UA: &str = "holonet/0.1";
const MAX_HOPS: usize = 10;
const MAX_BODY: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
struct StoredCookie {
    name: String,
    value: String,
    domain: String,
    host_only: bool,
    path: String,
    secure: bool,
}

/// Session cookies for the process. Not written to disk.
#[derive(Clone, Default)]
pub struct CookieJar {
    inner: Arc<Mutex<Vec<StoredCookie>>>,
}

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<StoredCookie>> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn header_for(&self, url: &str, source: &str, post: bool) -> Option<String> {
        let Ok(url) = url::Url::parse(url) else {
            return None;
        };
        let host = url.host_str()?.to_string();
        if post {
            let same = url::Url::parse(source)
                .ok()
                .and_then(|source| source.host_str().map(|h| h.eq_ignore_ascii_case(&host)))
                .unwrap_or(false);
            if !same {
                return None;
            }
        }
        let https = url.scheme() == "https";
        let path = url.path();
        let mut pairs = Vec::new();
        for cookie in self.lock().iter() {
            if cookie.secure && !https {
                continue;
            }
            let domain_ok = if cookie.host_only {
                host.eq_ignore_ascii_case(&cookie.domain)
            } else {
                host.eq_ignore_ascii_case(&cookie.domain)
                    || host
                        .to_ascii_lowercase()
                        .ends_with(&format!(".{}", cookie.domain.to_ascii_lowercase()))
            };
            if domain_ok && path_match(path, &cookie.path) {
                pairs.push(format!("{}={}", cookie.name, cookie.value));
            }
        }
        (!pairs.is_empty()).then(|| pairs.join("; "))
    }

    pub fn store_headers(&self, url: &str, headers: &HeaderMap) {
        let Ok(url) = url::Url::parse(url) else {
            return;
        };
        let mut jar = self.lock();
        for value in headers.get_all(SET_COOKIE) {
            let Ok(text) = value.to_str() else {
                continue;
            };
            if let Some(change) = parse_set_cookie(text, &url) {
                match change {
                    CookieChange::Delete { name, domain, path } => {
                        jar.retain(|existing| {
                            !(existing.name == name && existing.domain == domain && existing.path == path)
                        });
                    }
                    CookieChange::Set(cookie) => {
                        jar.retain(|existing| {
                            !(existing.name == cookie.name
                                && existing.domain == cookie.domain
                                && existing.path == cookie.path)
                        });
                        jar.push(cookie);
                    }
                }
            }
        }
    }
}

enum CookieChange {
    Set(StoredCookie),
    Delete { name: String, domain: String, path: String },
}

fn parse_set_cookie(header: &str, url: &url::Url) -> Option<CookieChange> {
    let mut parts = header.split(';');
    let (name, value) = parts.next()?.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut domain = String::new();
    // shortcut: missing Path becomes `/`, so a cookie is sent site-wide, upgrade if a form depends on a path-scoped cookie
    let mut path = "/".to_string();
    let mut secure = false;
    let mut host_only = true;
    let mut max_age: Option<i64> = None;
    let mut expires: Option<String> = None;
    for attr in parts {
        let attr = attr.trim();
        let (key, raw) = attr
            .split_once('=')
            .map(|(k, v)| (k.trim(), v.trim()))
            .unwrap_or((attr, ""));
        if key.eq_ignore_ascii_case("domain") && !raw.is_empty() {
            domain = raw.trim_start_matches('.').to_string();
            host_only = false;
        } else if key.eq_ignore_ascii_case("path") && raw.starts_with('/') {
            path = raw.to_string();
        } else if key.eq_ignore_ascii_case("secure") {
            secure = true;
        } else if key.eq_ignore_ascii_case("max-age") {
            max_age = raw.parse().ok();
        } else if key.eq_ignore_ascii_case("expires") && !raw.is_empty() {
            expires = Some(raw.to_string());
        }
    }
    if domain.is_empty() {
        domain = url.host_str()?.to_ascii_lowercase();
    } else if !domain_allowed(url.host_str()?, &domain) {
        return None;
    } else {
        domain = domain.to_ascii_lowercase();
    }
    let delete = match max_age {
        Some(age) => age <= 0,
        None => expires
            .as_deref()
            .and_then(http_date)
            .is_some_and(|secs| secs <= now_secs()),
    };
    if delete {
        return Some(CookieChange::Delete {
            name: name.to_string(),
            domain,
            path,
        });
    }
    Some(CookieChange::Set(StoredCookie {
        name: name.to_string(),
        value: value.trim().to_string(),
        domain,
        host_only,
        path,
        secure,
    }))
}

// shortcut: a public suffix such as co.uk is still accepted, upgrade when a site sets Domain on a suffix
fn domain_allowed(host: &str, domain: &str) -> bool {
    if !domain.contains('.') {
        return false;
    }
    let host = host.to_ascii_lowercase();
    let domain = domain.to_ascii_lowercase();
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn path_match(request: &str, cookie: &str) -> bool {
    request == cookie
        || (cookie.ends_with('/') && request.starts_with(cookie))
        || request.starts_with(&format!("{cookie}/"))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn http_date(value: &str) -> Option<i64> {
    let rest = value
        .split_once(',')
        .map(|(_, rest)| rest.trim())
        .unwrap_or(value.trim());
    let mut parts = rest.split_whitespace();
    let day: i64 = parts.next()?.parse().ok()?;
    let month = month_num(parts.next()?)?;
    let year: i64 = parts.next()?.parse().ok()?;
    let mut clock = parts.next()?.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

fn month_num(name: &str) -> Option<i64> {
    Some(match name {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    })
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = (year - era * 400) as u64;
    let month_prime = if month > 2 { month - 3 } else { month + 9 } as u64;
    let doy = (153 * month_prime + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

#[derive(Debug)]
pub struct Fetched {
    pub final_url: String,
    pub html: String,
}

pub fn client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client")
}

/// `Loader` for ordinary HTTP. Redirects are followed here so each hop can be blocked.
pub async fn fetch(
    client: &Client,
    blocker: &Blocker,
    jar: &CookieJar,
    url: &str,
    source: &str,
    post: bool,
    body: Option<String>,
) -> Result<Fetched, String> {
    let mut shown = url.to_string();
    let mut current = url.to_string();
    let mut source = if url::Url::parse(source).is_ok() {
        source.to_string()
    } else {
        url.to_string()
    };
    let mut post = post;
    let mut body = body;
    // shortcut: DuckDuckGo treats a compressed search as a bot, upgrade if a normal GET works again
    let mut plain_encoding = false;
    if !post && let Some((action, form)) = ddg_search_post(&current) {
        current = action;
        post = true;
        body = Some(form);
        plain_encoding = true;
    }
    for _ in 0..MAX_HOPS {
        if blocker.blocked(&current, &source, "document") {
            return Err("blocked".into());
        }
        let mut request = if post {
            client.post(&current)
        } else {
            client.get(&current)
        };
        request = request
            .header(USER_AGENT, UA)
            .header("accept", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8");
        if plain_encoding {
            request = request.header("accept-encoding", "identity");
            plain_encoding = false;
        }
        if let Some(cookie) = jar.header_for(&current, &source, post) {
            request = request.header(COOKIE, cookie);
        }
        if post && let Some(payload) = &body {
            request = request
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(payload.clone());
        }
        let response = request.send().await.map_err(|err| err.to_string())?;
        let status = response.status();
        let headers = response.headers().clone();
        jar.store_headers(&current, &headers);
        if status.is_redirection() {
            let location = headers
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "redirect without location".to_string())?;
            let next = join_url(&current, location)?;
            let keep_method = status == StatusCode::TEMPORARY_REDIRECT
                || status == StatusCode::PERMANENT_REDIRECT;
            if !keep_method {
                post = false;
                body = None;
            }
            source = current;
            current = next;
            shown = current.clone();
            continue;
        }
        if !status.is_success() {
            return Err(format!("http {status}"));
        }
        let content_type = headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = read_body(response).await?;
        return Ok(Fetched {
            final_url: shown,
            html: decode(&bytes, content_type.as_deref()),
        });
    }
    Err("too many redirects".into())
}

/// A DuckDuckGo lite or html search URL becomes a POST. Other DuckDuckGo URLs are left alone.
pub fn ddg_search_post(url: &str) -> Option<(String, String)> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    if host != "duckduckgo.com" && !host.ends_with(".duckduckgo.com") {
        return None;
    }
    let path = parsed.path().trim_end_matches('/');
    if path != "/lite" && path != "/html" {
        return None;
    }
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if !pairs.iter().any(|(key, value)| key == "q" && !value.is_empty()) {
        return None;
    }
    let action = if path == "/html" {
        "https://html.duckduckgo.com/html/"
    } else {
        "https://lite.duckduckgo.com/lite/"
    };
    let mut body = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in &pairs {
        body.append_pair(key, value);
    }
    Some((action.to_string(), body.finish()))
}

async fn read_body(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if buf.len() + chunk.len() > MAX_BODY {
                    return Err("page too large".into());
                }
                buf.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(buf),
            Err(err) => return Err(err.to_string()),
        }
    }
}

fn join_url(base: &str, location: &str) -> Result<String, String> {
    let base = url::Url::parse(base).map_err(|err| err.to_string())?;
    Ok(base
        .join(location)
        .map_err(|err| err.to_string())?
        .to_string())
}

/// Charset from the Content-Type header, then a `<meta charset>` scan, then UTF-8.
pub fn decode(bytes: &[u8], content_type: Option<&str>) -> String {
    let encoding = content_type
        .and_then(charset_from_header)
        .or_else(|| sniff_meta(bytes))
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

fn charset_from_header(content_type: &str) -> Option<&'static Encoding> {
    for part in content_type.split(';').skip(1) {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("charset") {
            let label = value.trim().trim_matches('"');
            return Encoding::for_label(label.as_bytes());
        }
    }
    None
}

// shortcut: meta charset is a byte search of the first 2 KiB, not the HTML prescan, upgrade if a page decodes as the wrong encoding
fn sniff_meta(bytes: &[u8]) -> Option<&'static Encoding> {
    let head = &bytes[..bytes.len().min(2048)];
    let lower = String::from_utf8_lossy(head).to_ascii_lowercase();
    let rest = lower.split("charset=").nth(1)?;
    let label: String = rest
        .chars()
        .skip_while(|c| *c == '"' || *c == '\'' || c.is_whitespace())
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    Encoding::for_label(label.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex as StdMutex;
    use std::thread;
    use std::time::{Duration, Instant};

    fn serve(max: usize, handler: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let handler = Arc::new(handler);
        thread::spawn(move || {
            let start = Instant::now();
            let mut served = 0;
            while served < max && start.elapsed() < Duration::from_secs(3) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        sock.set_read_timeout(Some(Duration::from_secs(1))).ok();
                        let mut buf = [0u8; 8192];
                        let n = sock.read(&mut buf).unwrap_or(0);
                        let request = String::from_utf8_lossy(&buf[..n]).to_string();
                        let _ = sock.write_all(&handler(&request));
                        served += 1;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        format!("http://{addr}")
    }

    fn response(status: u16, reason: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        let mut head = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (key, value) in headers {
            head.push_str(key);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn duckduckgo_search_is_a_post() {
        let (action, body) =
            ddg_search_post("https://html.duckduckgo.com/html/?q=rust+docs").unwrap();
        assert_eq!(action, "https://html.duckduckgo.com/html/");
        assert_eq!(body, "q=rust+docs");
        let (action, body) =
            ddg_search_post("https://lite.duckduckgo.com/lite/?q=rust%20docs").unwrap();
        assert_eq!(action, "https://lite.duckduckgo.com/lite/");
        assert_eq!(body, "q=rust+docs");
        let (_, body) = ddg_search_post("https://lite.duckduckgo.com/lite/?q=rust&s=10").unwrap();
        assert_eq!(body, "q=rust&s=10");
        assert!(ddg_search_post("https://duckduckgo.com/about?q=1").is_none());
        assert!(ddg_search_post("https://example.org/?q=rust").is_none());
    }

    #[test]
    fn decodes_declared_charset() {
        let text = decode(&[0xE9], Some("text/html; charset=windows-1252"));
        assert_eq!(text, "é");
        let text = decode(&[0xE9], Some("text/html; charset=windows-1252; x"));
        assert_eq!(text, "é");
    }

    #[tokio::test]
    async fn does_not_connect_when_the_document_is_blocked() {
        let blocker = Blocker::from_rules("||evil.test^\n");
        let err = fetch(
            &client(),
            &blocker,
            &CookieJar::new(),
            "https://evil.test/ad",
            "https://example.com/",
            false,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "blocked");
    }

    #[tokio::test]
    async fn follows_redirects_and_sends_cookies() {
        let seen = Arc::new(StdMutex::new(String::new()));
        let seen2 = seen.clone();
        let base = serve(4, move |request| {
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            if path.starts_with("/a") {
                response(
                    302,
                    "Found",
                    &[("location", "/b"), ("set-cookie", "sid=1; Path=/")],
                    b"",
                )
            } else {
                let cookie = request.lines().find_map(|line| {
                    line.to_ascii_lowercase()
                        .starts_with("cookie:")
                        .then(|| line.to_string())
                });
                *seen2.lock().unwrap() = cookie.unwrap_or_default();
                response(
                    200,
                    "OK",
                    &[("content-type", "text/html; charset=utf-8")],
                    b"<p>hi</p>",
                )
            }
        });
        let blocker = Blocker::from_rules("");
        let fetched = fetch(
            &client(),
            &blocker,
            &CookieJar::new(),
            &format!("{base}/a"),
            &base,
            false,
            None,
        )
        .await
        .unwrap();
        assert!(fetched.final_url.ends_with("/b"), "{}", fetched.final_url);
        assert!(fetched.html.contains("hi"));
        assert!(
            seen.lock().unwrap().to_ascii_lowercase().contains("sid=1"),
            "{}",
            seen.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn blocks_a_redirect_hop() {
        let base = serve(2, |_request| {
            response(302, "Found", &[("location", "http://evil.test/ad")], b"")
        });
        let blocker = Blocker::from_rules("||evil.test^\n");
        let err = fetch(
            &client(),
            &blocker,
            &CookieJar::new(),
            &format!("{base}/go"),
            &base,
            false,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "blocked");
    }

    fn jar_with(url: &str, cookie: &str) -> CookieJar {
        let jar = CookieJar::new();
        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            reqwest::header::HeaderValue::from_str(cookie).unwrap(),
        );
        jar.store_headers(url, &headers);
        jar
    }

    #[test]
    fn cookie_domain_path_and_logout() {
        let planted = jar_with("https://evil.test/", "sid=1; Domain=example.com");
        assert!(
            planted
                .header_for("https://example.com/", "https://example.com/", false)
                .is_none()
        );

        let bare = jar_with("https://example.com/", "sid=1; Domain=com");
        assert!(
            bare.header_for("https://example.com/", "https://example.com/", false)
                .is_none()
        );

        let jar = jar_with("https://example.com/docs/page", "sid=1; Path=/docs");
        let sent = jar
            .header_for("https://example.com/docs/a", "https://example.com/", false)
            .unwrap();
        assert!(sent.contains("sid=1"), "{sent}");
        assert!(
            jar.header_for(
                "https://example.com/docs-private",
                "https://example.com/",
                false
            )
            .is_none()
        );

        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            reqwest::header::HeaderValue::from_str("sid=; Max-Age=0; Path=/docs").unwrap(),
        );
        jar.store_headers("https://example.com/docs/page", &headers);
        assert!(
            jar.header_for("https://example.com/docs/a", "https://example.com/", false)
                .is_none()
        );

        let expired = jar_with(
            "https://example.com/",
            "old=1; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
        );
        assert!(
            expired
                .header_for("https://example.com/", "https://example.com/", false)
                .is_none()
        );
    }

    #[test]
    fn post_from_another_site_sends_no_cookies() {
        let jar = jar_with("https://bank.test/login", "sid=1");
        assert!(
            jar.header_for("https://bank.test/transfer", "https://evil.test/", true)
                .is_none()
        );
        let sent = jar
            .header_for("https://bank.test/transfer", "https://bank.test/", true)
            .unwrap();
        assert!(sent.contains("sid=1"), "{sent}");
    }

    #[tokio::test]
    async fn stops_reading_a_page_past_the_cap() {
        let body = vec![b'a'; MAX_BODY + 1];
        let base = serve(1, move |_request| response(200, "OK", &[], &body));
        let err = fetch(
            &client(),
            &Blocker::from_rules(""),
            &CookieJar::new(),
            &base,
            &base,
            false,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "page too large");
    }
}
