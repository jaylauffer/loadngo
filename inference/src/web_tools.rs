//! Web tools a local model may call: search and read public pages, as text.
//!
//! - `web_search`: DuckDuckGo's plain-HTML results page (no account or key), parsed
//!   into title, address and snippet.
//! - `web_fetch`: one public `http(s)` page, converted to plain text, returned in
//!   6 KiB windows.
//!
//! These are the only tools that leave the machine: the query or address is sent to
//! the site. Everything is bounded (time, bytes, results) and read-only. Addresses
//! that resolve to this machine or the local network (loopback, private, link-local,
//! carrier-grade NAT) are refused, including after a redirect, so a page cannot steer
//! the model into the lab's own services.

use std::fmt::Write as _;
use std::io::Read as _;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use serde_json::{json, Value};

use crate::tools::{str_arg, usize_arg, Tool};

/// Text returned by one `web_fetch` call. Smaller than a file read: pages are long, and
/// every byte is prompt a local model must read before it can answer.
pub const MAX_PAGE_BYTES: usize = 6 * 1024;

/// Largest response body read, before conversion to text.
const MAX_BODY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_RESULTS: usize = 8;
const MAX_REDIRECTS: usize = 5;
const TIMEOUT: Duration = Duration::from_secs(20);
const USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) \
     Version/18.0 Safari/605.1.15";

/// The two web tools, sharing one HTTP agent.
pub struct WebTools {
    agent: ureq::Agent,
}

impl Default for WebTools {
    fn default() -> Self {
        Self::new()
    }
}

impl WebTools {
    pub fn new() -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(TIMEOUT)
                .redirects(0)
                .user_agent(USER_AGENT)
                .build(),
        }
    }

    pub fn into_tools(self) -> Vec<Box<dyn Tool>> {
        let shared = std::sync::Arc::new(self);
        vec![
            Box::new(WebSearch(std::sync::Arc::clone(&shared))),
            Box::new(WebFetch(shared)),
        ]
    }

    /// GETs a public address, following at most [`MAX_REDIRECTS`] redirects, each one
    /// checked; returns the final address, content type and body (bounded).
    fn get(&self, address: &str) -> Result<(String, String, Vec<u8>), String> {
        let mut address = address.to_string();
        for _ in 0..=MAX_REDIRECTS {
            let url = public_url(&address)?;
            let response = match self.agent.get(url.as_str()).call() {
                Ok(r) => r,
                Err(ureq::Error::Status(code, r)) if (300..400).contains(&code) => r,
                Err(ureq::Error::Status(code, _)) => {
                    return Err(format!("{address}: the site answered HTTP {code}"))
                }
                Err(e) => return Err(format!("{address}: {e}")),
            };
            if (300..400).contains(&response.status()) {
                let next = response
                    .header("location")
                    .ok_or_else(|| format!("{address}: redirect without a location"))?;
                address = url
                    .join(next)
                    .map_err(|e| format!("{address}: bad redirect {next:?}: {e}"))?
                    .to_string();
                continue;
            }
            let kind = response.content_type().to_string();
            let mut body = Vec::new();
            response
                .into_reader()
                .take(MAX_BODY_BYTES)
                .read_to_end(&mut body)
                .map_err(|e| format!("{address}: {e}"))?;
            return Ok((address, kind, body));
        }
        Err(format!("{address}: more than {MAX_REDIRECTS} redirects"))
    }
}

/// Parses `address` and refuses anything but `http(s)` to a public host.
fn public_url(address: &str) -> Result<url::Url, String> {
    let url =
        url::Url::parse(address).map_err(|e| format!("{address:?} is not an address: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "only http and https pages can be fetched, not {}",
            url.scheme()
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| format!("{address} has no host"))?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<IpAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("{host}: {e}"))?
        .map(|a| a.ip())
        .collect();
    if addrs.is_empty() {
        return Err(format!("{host} does not resolve"));
    }
    if let Some(ip) = addrs.iter().find(|ip| !is_public(ip)) {
        return Err(format!(
            "{host} is on this machine or the local network ({ip}); only public sites can be fetched"
        ));
    }
    Ok(url)
}

/// False for loopback, private, link-local, carrier-grade NAT, unspecified, multicast
/// and documentation addresses.
pub fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || (a == 100 && (64..128).contains(&b)))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(&IpAddr::V4(v4));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 // unique local
                || (first & 0xffc0) == 0xfe80) // link-local
        }
    }
}

struct WebSearch(std::sync::Arc<WebTools>);
struct WebFetch(std::sync::Arc<WebTools>);

impl Tool for WebSearch {
    fn name(&self) -> &'static str {
        "web_search"
    }
    fn description(&self) -> &'static str {
        "Search the public web (DuckDuckGo). Returns up to 8 results: title, address and a snippet. Use web_fetch to read a result."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "query": {"type": "string", "description": "what to search for"}},
            "required": ["query"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let query = str_arg(args, "query")?.trim();
        if query.is_empty() {
            return Err("query is empty".into());
        }
        let mut address =
            url::Url::parse("https://html.duckduckgo.com/html/").map_err(|e| e.to_string())?;
        address.query_pairs_mut().append_pair("q", query);
        let (_, _, body) = self.0.get(address.as_str())?;
        let page = String::from_utf8_lossy(&body);
        let results = parse_duckduckgo(&page);
        if results.is_empty() {
            return Ok(format!("no results for {query:?}"));
        }
        let mut out = format!("results for {query:?}:\n");
        for (i, r) in results.iter().enumerate() {
            let _ = writeln!(
                out,
                "{}. {}\n   {}\n   {}",
                i + 1,
                r.title,
                r.url,
                r.snippet
            );
        }
        Ok(out)
    }
}

impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        "web_fetch"
    }
    fn description(&self) -> &'static str {
        "Read a public web page as plain text, about 6 KB per call; pass start to continue where the last call ended."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "url": {"type": "string", "description": "http or https address"},
            "start": {"type": "integer", "description": "character offset to start at (default 0)"}},
            "required": ["url"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        let (address, kind, body) = self.0.get(str_arg(args, "url")?)?;
        let raw = String::from_utf8_lossy(&body);
        let text = if kind.contains("html") || raw.trim_start().starts_with('<') {
            let title = element(&raw, "title").map(html_to_text);
            let body = html_to_text(main_content(&raw));
            match title {
                Some(t) if !t.is_empty() && !body.starts_with(&t) => format!("{t}\n{body}"),
                _ => body,
            }
        } else if kind.starts_with("text/") || kind.contains("json") || kind.contains("xml") {
            raw.into_owned()
        } else {
            return Err(format!(
                "{address} is {kind}, not text ({} bytes)",
                body.len()
            ));
        };
        let chars: Vec<char> = text.chars().collect();
        let start = usize_arg(args, "start", 0).min(chars.len());
        let mut end = start;
        let mut bytes = 0;
        while end < chars.len() && bytes + chars[end].len_utf8() <= MAX_PAGE_BYTES {
            bytes += chars[end].len_utf8();
            end += 1;
        }
        let window: String = chars[start..end].iter().collect();
        let mut out = format!(
            "{address} ({} characters of text; showing {start}..{end})\n\n{window}",
            chars.len()
        );
        if end < chars.len() {
            let _ = write!(out, "\n\n[more: call web_fetch with start={end}]");
        }
        Ok(out)
    }
}

/// The inside of the first `<name ...>...</name>` element, if the page has one.
fn element<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find(&format!("<{name}"))?;
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find(&format!("</{name}"))?;
    Some(&html[start..end])
}

/// The page's main content when it marks it (`<main>`, else `<article>`), without the
/// site's menus and navigation; otherwise the whole page.
fn main_content(html: &str) -> &str {
    element(html, "main")
        .or_else(|| element(html, "article"))
        .unwrap_or(html)
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Results from DuckDuckGo's HTML page: `a.result__a` (title, redirect address) and the
/// following `result__snippet`. Ads (addresses through `duckduckgo.com/y.js`) are skipped.
pub fn parse_duckduckgo(page: &str) -> Vec<SearchResult> {
    let mut out = Vec::new();
    let mut rest = page;
    while let Some(at) = rest.find("class=\"result__a\"") {
        let after = &rest[at..];
        let Some(tag_end) = after.find('>') else {
            break;
        };
        let tag = &after[..tag_end];
        let Some(close) = after[tag_end..].find("</a>") else {
            break;
        };
        let title = html_to_text(&after[tag_end + 1..tag_end + close]);
        let href = attribute(tag, "href").unwrap_or_default();
        let next = after[1..]
            .find("class=\"result__a\"")
            .map_or(after.len(), |n| n + 1);
        let block = &after[..next];
        let snippet = block
            .find("class=\"result__snippet\"")
            .and_then(|s| {
                let from = &block[s..];
                let open = from.find('>')?;
                let end = from[open..]
                    .find("</a>")
                    .or_else(|| from[open..].find("</div>"))?;
                Some(html_to_text(&from[open + 1..open + end]))
            })
            .unwrap_or_default();
        rest = &after[next..];
        let url = result_address(&href);
        if url.is_empty() || url.contains("duckduckgo.com/y.js") {
            continue;
        }
        out.push(SearchResult {
            title: title.trim().to_string(),
            url,
            snippet: snippet.trim().to_string(),
        });
        if out.len() == MAX_RESULTS || next >= after.len() {
            break;
        }
    }
    out
}

/// The target of a DuckDuckGo redirect (`//duckduckgo.com/l/?uddg=<encoded>&rut=...`),
/// or the address itself.
fn result_address(href: &str) -> String {
    let href = decode_entities(href);
    if let Some(at) = href.find("uddg=") {
        let encoded = href[at + 5..].split('&').next().unwrap_or("");
        return percent_decode(encoded);
    }
    if href.starts_with("//") {
        return format!("https:{href}");
    }
    href
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let at = tag.find(&format!("{name}=\""))?;
    let from = &tag[at + name.len() + 2..];
    Some(from[..from.find('"')?].to_string())
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decodes the HTML entities pages actually use: the named basics and numeric ones.
pub fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        // Byte search: ';' is ASCII, so its index is always a char boundary, while a
        // fixed 12-byte slice can end inside a multibyte character.
        let Some(semi) = after.bytes().take(12).position(|b| b == b';') else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "ndash" => Some('–'),
            "mdash" => Some('—'),
            "hellip" => Some('…'),
            "rsquo" | "lsquo" => Some('\''),
            "rdquo" | "ldquo" => Some('"'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Plain text from HTML: scripts, styles and other non-content elements dropped, block
/// elements on their own lines, entities decoded, whitespace and blank lines collapsed.
pub fn html_to_text(html: &str) -> String {
    const SKIP: [&str; 9] = [
        "script", "style", "noscript", "svg", "template", "iframe", "nav", "aside", "button",
    ];
    const BLOCK: [&str; 22] = [
        "p",
        "br",
        "div",
        "li",
        "ul",
        "ol",
        "tr",
        "td",
        "th",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "section",
        "article",
        "header",
        "footer",
        "table",
        "blockquote",
        "pre",
    ];
    let mut text = String::with_capacity(html.len() / 2);
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let after = &rest[open..];
        if after.starts_with("<!--") {
            rest = after.find("-->").map_or("", |e| &after[e + 3..]);
            continue;
        }
        let Some(close) = after.find('>') else {
            rest = "";
            break;
        };
        let tag = after[1..close].trim_start_matches('/');
        let name: String = tag
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        rest = &after[close + 1..];
        if SKIP.contains(&name.as_str()) && !after[1..].starts_with('/') {
            let end = format!("</{name}");
            let lower = rest.to_ascii_lowercase();
            rest = lower
                .find(&end)
                .and_then(|e| rest[e..].find('>').map(|g| &rest[e + g + 1..]))
                .unwrap_or("");
            continue;
        }
        if BLOCK.contains(&name.as_str()) {
            text.push('\n');
        }
    }
    text.push_str(rest);
    let text = decode_entities(&text);
    // One line per block, no blank lines: the reader is a model with a small context.
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !line.is_empty() {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duckduckgo_results_parse_to_title_address_and_snippet() {
        let page = r#"<div class="result results_links"><h2 class="result__title">
<a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fen.wikipedia.org%2Fwiki%2FBangkok&amp;rut=abc">Bangkok &amp; its <b>floods</b></a></h2>
<a class="result__snippet" href="x">Bangkok is the <b>capital</b> of Thailand.</a></div>
<div class="result result--ad"><a class="result__a" href="https://duckduckgo.com/y.js?ad=1">Ad</a></div>
<div class="result"><a class="result__a" href="https://example.org/two">Second</a>
<a class="result__snippet">Two.</a></div>"#;
        let results = parse_duckduckgo(page);
        assert_eq!(results.len(), 2, "{results:?}");
        assert_eq!(results[0].title, "Bangkok & its floods");
        assert_eq!(results[0].url, "https://en.wikipedia.org/wiki/Bangkok");
        assert_eq!(results[0].snippet, "Bangkok is the capital of Thailand.");
        assert_eq!(results[1].url, "https://example.org/two");
    }

    #[test]
    fn html_becomes_readable_text() {
        let html = "<html><head><style>p{color:red}</style><script>var x = '<p>';</script></head>\
            <body><h1>Title</h1><p>One &amp; two&nbsp;three</p><!-- note --><ul><li>a</li><li>b</li></ul>\
            <p>caf&#233; &#x2014; done</p></body></html>";
        assert_eq!(
            html_to_text(html),
            "Title\nOne & two three\na\nb\ncafé — done"
        );
    }

    #[test]
    fn a_bare_ampersand_before_multibyte_text_is_kept() {
        // The page that crashed Kimi: no ';' after a '&', and a character wider than one
        // byte straddling the 12-byte entity window ('’' at bytes 10..13).
        assert_eq!(
            decode_entities("Tom &amp Jerry’s; &amp;"),
            "Tom &amp Jerry’s; &"
        );
        assert_eq!(decode_entities("&123456789é"), "&123456789é");
    }

    #[test]
    fn local_and_private_addresses_are_refused() {
        for ip in [
            "127.0.0.1",
            "10.10.10.6",
            "192.168.1.160",
            "172.16.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
        ] {
            assert!(!is_public(&ip.parse().unwrap()), "{ip} should be refused");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(is_public(&ip.parse().unwrap()), "{ip} should be allowed");
        }
        assert!(public_url("http://127.0.0.1:8080/")
            .unwrap_err()
            .contains("local network"));
        assert!(public_url("file:///etc/passwd")
            .unwrap_err()
            .contains("only http"));
    }

    /// Live: searches and fetches over the network. Run by hand with
    /// `cargo test -p loadngo-inference --features web live_web -- --ignored --nocapture`.
    #[test]
    #[ignore = "uses the network"]
    fn live_web_search_and_fetch() {
        let tools = WebTools::new().into_tools();
        let search = tools[0]
            .call(&json!({"query": "Bangkok flood September 2026"}))
            .unwrap();
        println!("{search}");
        assert!(search.contains("1. "), "no results parsed");
        let page = tools[1]
            .call(&json!({"url": "https://en.wikipedia.org/wiki/Lat_Phrao_district"}))
            .unwrap();
        println!("{}", page.chars().take(1200).collect::<String>());
        assert!(page.contains("Lat Phrao"));
    }
}
