//! Why a page did not come, read off what the engine already says.
//!
//! A navigation can fail in two ways, and only one of them is an error on the
//! wire. `Page.navigate` to a url the engine cannot parse is a CDP error, and
//! [`crate::cdp`] turns that into a sentence already. Everything else — a host
//! that does not resolve, a port nobody listens on, a server speaking plain
//! HTTP to a `https://` url — is a *successful* command: the engine did
//! navigate, to its own error page, and says so in the reply. Before this
//! module the program took the reply as good news and then showed the error
//! page's address, `chrome-error://chromewebdata/`, as the page's url.
//!
//! # What the engine says when a page does not come
//!
//! Measured against `headless_shell` 141.0.7390.37 with `Page.enable` and
//! nothing else, for a closed port, a `.invalid` host, `https://` at a
//! plain-HTTP port, and a 302 into a closed port:
//!
//! ```text
//! +10 ms      reply to Page.navigate: {"frameId", "loaderId",
//!             "errorText":"net::ERR_CONNECTION_REFUSED", "isDownload":false}
//! +10-60 ms   Page.frameNavigated, main frame: frame.url is
//!             "chrome-error://chromewebdata/" and frame.unreachableUrl is
//!             the url that failed (for a redirect, where it ended)
//! +1 ms       Target.targetInfoChanged on the browser connection: the failed
//!             url in the engine's spelling, and a title made of its host
//! +2 ms       Page.domContentEventFired, Page.loadEventFired,
//!             Page.frameStoppedLoading — the error page loads like a page
//! ```
//!
//! A `.invalid` host is `net::ERR_NAME_NOT_RESOLVED` in those same ten
//! milliseconds; the resolver refuses the reserved name without asking anyone.
//!
//! So the *reason* comes from one place, the reply to `Page.navigate`, and the
//! *fact* of the failure from another, `frame.unreachableUrl`. The second is
//! the one that matters more: the same `Page.frameNavigated` with the same
//! field arrives for a link that was clicked (30 ms after the click), for a
//! `Page.reload` of an error page (6 ms), and for a walk through history, none
//! of which went through `Page.navigate` and none of which has a reply to
//! read. A failure found that way names the host and not the reason, because
//! nothing free says what the reason was. The error page itself does not:
//! its `outerHTML` is an empty body and its `document.title` is empty.
//!
//! # What it would cost to know more, and why that is not paid
//!
//! The `Network` domain would say why every failed load failed —
//! `Network.loadingFailed` has the same `errorText` — and it is not enabled.
//! On a page of 51 requests it is 341 `Network.*` events and about 212 kB of
//! JSON, against 6 `Page.*` events and 1 kB for the same load. Every one of
//! them goes through the tab's mailbox, which holds 512 events and drops the
//! oldest when it is full, and what would be dropped is exactly what this
//! program lives on: `frameNavigated`, `loadEventFired`, and screencast frames
//! whose acknowledgements are what keep the next frame coming. The seam is
//! there if it is ever worth it — a `Network.loadingFailed` for the main
//! frame's document would call [`crate::tabs::Tab::failed_to_reach`] exactly
//! as a `Page.navigate` reply does.
//!
//! The `Log` domain was tried too. It reports a main document's 404, and
//! nothing at all for a main-frame network error, so it answers the question
//! that already had an answer and not the one that did not.
//!
//! # A status without the Network domain
//!
//! A 404 is not a failure to the engine: the page loads, with whatever body
//! the server sent, and there is no `errorText`. The status is still on the
//! page, though, in the navigation timing entry:
//! `performance.getEntriesByType('navigation')[0].responseStatus` is 404, 500
//! or 200 for the document; 0 on an error page and on `about:blank`; the final
//! document's after a redirect; and it survives a reload and a `pushState`.
//! The program already asks every page for its title when it has loaded, and
//! [`LOADED`] asks for both in the one `Runtime.evaluate`, so knowing the
//! status costs nothing that was not already being spent.
//!
//! # What is not here
//!
//! A way past a certificate error. The error is said — the `ERR_CERT_*` codes
//! have words below — but going on anyway is out of scope:
//! `Security.setIgnoreCertificateErrors` exists, and it applies to the whole
//! target, every host that tab visits afterwards and not just the one somebody
//! decided to trust. Doing that properly wants a question asked of the person
//! each time, which is a prompt this program does not have yet.

use crate::json::Json;

/// Where the main frame ended up after a navigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landing {
    /// A document, at this url.
    Document(String),
    /// The engine's error page, standing in for this url, which did not come.
    Unreachable(String),
}

/// Something wrong with the page in a tab that is worth a word on the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The page did not come at all.
    Unreachable {
        /// Where it was supposed to come from: the url the engine ended up
        /// at, in its own spelling, once the error page has landed.
        url: String,
        /// The engine's `net::ERR_` code, when a `Page.navigate` reply gave
        /// one; `None` when the page found out on its own — a link, a reload,
        /// history — and nothing said why.
        reason: Option<String>,
    },
    /// The page came, and the server said it was an error while sending it.
    Status(u16),
}

/// The reason in a `Page.navigate` reply, if the navigation failed.
///
/// `None` for a reply with no `errorText`, an empty one, or
/// `net::ERR_ABORTED` — which is not a failure but a navigation that was
/// superseded by another before it committed, or a url that turned out to be
/// a download. Neither is anything the person needs telling about.
pub fn failed(reply: &Json) -> Option<String> {
    let text = reply.get("errorText").and_then(Json::as_str)?;
    if text.is_empty() || text == "net::ERR_ABORTED" {
        return None;
    }
    Some(text.to_string())
}

/// Where a `Page.frameNavigated` says the main frame went.
///
/// `None` for a frame with a parent — an iframe navigating is an advert
/// changing, not the page — and for anything that is not the shape the engine
/// sends. The error page's own url, `chrome-error://chromewebdata/`, is never
/// what comes out: when `unreachableUrl` is there, it is the address.
pub fn landing(params: &Json) -> Option<Landing> {
    let frame = params.get("frame")?;
    if frame.get("parentId").is_some() {
        return None;
    }
    if let Some(unreachable) = frame.get("unreachableUrl").and_then(Json::as_str) {
        if !unreachable.is_empty() {
            return Some(Landing::Unreachable(unreachable.to_string()));
        }
    }
    let url = frame.get("url").and_then(Json::as_str)?;
    Some(Landing::Document(url.to_string()))
}

/// A `net::ERR_` code in words.
///
/// Three of these were measured — `NAME_NOT_RESOLVED`, `CONNECTION_REFUSED`
/// and `SSL_PROTOCOL_ERROR` — and the rest are the codes in Chromium's
/// `net/base/net_error_list.h` that a person meets on the open web, worded
/// for a status row rather than for a developer. A code that is not in the
/// table is still said rather than dropped: `net::ERR_SOMETHING_NEW` reads as
/// "something new", which is usually enough to search for.
pub fn reason(code: &str) -> String {
    let bare = code.strip_prefix("net::ERR_").unwrap_or(code);
    let words = match bare {
        "NAME_NOT_RESOLVED" => "name not resolved",
        "INTERNET_DISCONNECTED" => "no network",
        "CONNECTION_REFUSED" => "connection refused",
        "CONNECTION_RESET" => "connection reset",
        "CONNECTION_CLOSED" => "connection closed",
        "CONNECTION_ABORTED" => "connection aborted",
        "CONNECTION_FAILED" => "connection failed",
        "CONNECTION_TIMED_OUT" | "TIMED_OUT" => "timed out",
        "ADDRESS_UNREACHABLE" => "address unreachable",
        "NETWORK_UNREACHABLE" => "network unreachable",
        "EMPTY_RESPONSE" => "empty response",
        "INVALID_RESPONSE" => "invalid response",
        "INVALID_HTTP_RESPONSE" => "not an http response",
        "TOO_MANY_REDIRECTS" => "too many redirects",
        "SSL_PROTOCOL_ERROR" => "not speaking tls",
        "SSL_VERSION_OR_CIPHER_MISMATCH" => "no tls version in common",
        "CERT_AUTHORITY_INVALID" => "certificate not trusted",
        "CERT_COMMON_NAME_INVALID" => "certificate is for another name",
        "CERT_DATE_INVALID" => "certificate expired or not yet valid",
        "CERT_REVOKED" => "certificate revoked",
        "CERT_INVALID" => "certificate invalid",
        "PROXY_CONNECTION_FAILED" | "TUNNEL_CONNECTION_FAILED" => "the proxy would not connect",
        "BLOCKED_BY_CLIENT" => "blocked",
        "BLOCKED_BY_RESPONSE" => "blocked by the site",
        "FILE_NOT_FOUND" => "no such file",
        "ACCESS_DENIED" => "access denied",
        "UNKNOWN_URL_SCHEME" => "unknown url scheme",
        "INVALID_URL" => "invalid url",
        "HTTP2_PROTOCOL_ERROR" => "http/2 protocol error",
        "QUIC_PROTOCOL_ERROR" => "quic protocol error",
        other => return other.replace('_', " ").to_lowercase(),
    };
    words.to_string()
}

/// What a failure is a failure to reach: the host, with its port when it has
/// one, or the path of a file.
///
/// Not the whole url. The path and the query of a page that did not come are
/// not what went wrong — the host did — and the row is one line. Userinfo is
/// dropped because it is nobody's business on a status row.
pub fn subject(url: &str) -> String {
    if let Some(path) = url.strip_prefix("file://") {
        let end = path.find(['?', '#']).unwrap_or(path.len());
        return path[..end].to_string();
    }
    let Some((_, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    host.to_string()
}

/// The sentence the row says for a problem.
///
/// `can't reach example.cmo: name not resolved` when the reason is known,
/// `can't reach example.cmo` when the page found out by itself, and
/// `can't open /etc/x: no such file` for a file, which is opened rather than
/// reached.
pub fn sentence(problem: &Problem) -> String {
    match problem {
        Problem::Unreachable { url, reason: why } => {
            let verb = if url.starts_with("file:") {
                "open"
            } else {
                "reach"
            };
            let subject = subject(url);
            match why {
                Some(code) => format!("can't {verb} {subject}: {}", reason(code)),
                None => format!("can't {verb} {subject}"),
            }
        }
        Problem::Status(status) => status_phrase(*status),
    }
}

/// An HTTP error status, with a word or two when it is a common one.
///
/// The number always comes first, because it is what somebody searches for;
/// the words are there for the ones everybody knows by sight anyway.
pub fn status_phrase(status: u16) -> String {
    let words = match status {
        400 => "bad request",
        401 => "unauthorized",
        403 => "forbidden",
        404 => "not found",
        405 => "method not allowed",
        410 => "gone",
        429 => "too many requests",
        500 => "server error",
        502 => "bad gateway",
        503 => "unavailable",
        504 => "gateway timeout",
        _ => return status.to_string(),
    };
    format!("{status} {words}")
}

/// What a page is asked when it says it has loaded: its title, and the status
/// its document came with.
///
/// One expression rather than two, so that the status rides in the
/// `Runtime.evaluate` the title was already costing. The `try` is for a page
/// that has replaced `performance` with something of its own; a page that
/// breaks the status still gets its title read.
pub const LOADED: &str =
    "(function(){var s=0;try{var n=performance.getEntriesByType('navigation')[0];\
s=n?n.responseStatus:0}catch(e){}return [document.title,s]})()";

/// What [`LOADED`] answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub title: String,
    /// The document's status, only when it is an error: a 200 is not news,
    /// and a 0 is a page that had no response to have a status — an error
    /// page, `about:blank`, a `data:` url.
    pub status: Option<u16>,
}

/// Read the reply to a `Runtime.evaluate` of [`LOADED`], made with
/// `returnByValue`, without which an array comes back as a handle.
///
/// `None` when there is no title in it, which is a page that did not answer
/// the question that was asked.
pub fn loaded(reply: &Json) -> Option<Loaded> {
    let value = reply.path(&["result", "value"])?.as_array()?;
    let title = value.first()?.as_str()?.to_string();
    let status = value
        .get(1)
        .and_then(Json::as_f64)
        .filter(|status| (400.0..1000.0).contains(status))
        .map(|status| status as u16);
    Some(Loaded { title, status })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Json {
        Json::parse(text).expect("the test's own JSON")
    }

    #[test]
    fn the_error_text_comes_out_of_a_navigate_reply_and_a_clean_reply_has_none() {
        let failed_reply = json(
            r#"{"frameId":"F","loaderId":"L","errorText":"net::ERR_NAME_NOT_RESOLVED","isDownload":false}"#,
        );
        assert_eq!(
            failed(&failed_reply),
            Some("net::ERR_NAME_NOT_RESOLVED".to_string())
        );
        assert_eq!(failed(&json(r#"{"frameId":"F","loaderId":"L"}"#)), None);
        assert_eq!(
            failed(&json(r#"{"frameId":"F","errorText":""}"#)),
            None,
            "an empty reason is no reason"
        );
        assert_eq!(
            failed(&json(r#"{"frameId":"F","errorText":"net::ERR_ABORTED"}"#)),
            None,
            "a navigation that was overtaken, or a download, is not a failure"
        );
    }

    #[test]
    fn a_net_error_reads_as_words_and_an_unknown_one_as_its_code() {
        assert_eq!(reason("net::ERR_NAME_NOT_RESOLVED"), "name not resolved");
        assert_eq!(reason("net::ERR_CONNECTION_REFUSED"), "connection refused");
        assert_eq!(reason("net::ERR_SSL_PROTOCOL_ERROR"), "not speaking tls");
        assert_eq!(reason("net::ERR_TIMED_OUT"), "timed out");
        assert_eq!(reason("net::ERR_CONNECTION_TIMED_OUT"), "timed out");
        assert_eq!(
            reason("net::ERR_CERT_AUTHORITY_INVALID"),
            "certificate not trusted"
        );
        assert_eq!(reason("net::ERR_SOMETHING_NEW"), "something new");
        assert_eq!(
            reason("SOMETHING_ELSE"),
            "something else",
            "and a code without the prefix is still words"
        );
    }

    #[test]
    fn the_main_frames_landing_is_read_and_an_iframes_is_not() {
        // Exactly what the engine sent for a navigation to a closed port.
        let error_page = json(
            r#"{"frame":{"id":"854F","loaderId":"D8DD","url":"chrome-error://chromewebdata/",
                "domainAndRegistry":"","securityOrigin":"://","mimeType":"text/html",
                "unreachableUrl":"http://127.0.0.1:58465/","secureContextType":"InsecureScheme"},
                "type":"Navigation"}"#,
        );
        assert_eq!(
            landing(&error_page),
            Some(Landing::Unreachable("http://127.0.0.1:58465/".to_string()))
        );

        let document = json(
            r#"{"frame":{"id":"854F","loaderId":"D8DE","url":"http://127.0.0.1:58466/",
                "securityOrigin":"http://127.0.0.1:58466","mimeType":"text/html"},
                "type":"Navigation"}"#,
        );
        assert_eq!(
            landing(&document),
            Some(Landing::Document("http://127.0.0.1:58466/".to_string()))
        );

        let iframe = json(
            r#"{"frame":{"id":"99","parentId":"854F","url":"chrome-error://chromewebdata/",
                "unreachableUrl":"http://ads.invalid/"},"type":"Navigation"}"#,
        );
        assert_eq!(landing(&iframe), None, "an iframe is not the page");
        assert_eq!(landing(&json(r#"{"type":"Navigation"}"#)), None);
    }

    #[test]
    fn the_subject_of_a_failure_is_the_host_or_the_file() {
        assert_eq!(
            subject("https://user@example.cmo:8443/x?y"),
            "example.cmo:8443"
        );
        assert_eq!(subject("file:///etc/x"), "/etc/x");
        assert_eq!(
            subject("https://nonexistent.invalid"),
            "nonexistent.invalid"
        );
        assert_eq!(
            subject("https://nonexistent.invalid/"),
            "nonexistent.invalid"
        );
        assert_eq!(subject("http://127.0.0.1:1#top"), "127.0.0.1:1");

        let file = Problem::Unreachable {
            url: "file:///etc/x".to_string(),
            reason: Some("net::ERR_FILE_NOT_FOUND".to_string()),
        };
        assert_eq!(sentence(&file), "can't open /etc/x: no such file");
        let host = Problem::Unreachable {
            url: "https://example.cmo/".to_string(),
            reason: Some("net::ERR_NAME_NOT_RESOLVED".to_string()),
        };
        assert_eq!(
            sentence(&host),
            "can't reach example.cmo: name not resolved"
        );
        let unknown = Problem::Unreachable {
            url: "https://example.cmo/".to_string(),
            reason: None,
        };
        assert_eq!(sentence(&unknown), "can't reach example.cmo");
    }

    #[test]
    fn the_title_and_the_status_come_out_of_one_evaluation() {
        let answer = |value: &str| {
            loaded(&json(&format!(
                r#"{{"result":{{"type":"object","value":{value}}}}}"#
            )))
        };
        assert_eq!(
            answer(r#"["nope",404]"#),
            Some(Loaded {
                title: "nope".to_string(),
                status: Some(404)
            })
        );
        assert_eq!(
            answer(r#"["fine",200]"#),
            Some(Loaded {
                title: "fine".to_string(),
                status: None
            })
        );
        assert_eq!(
            answer(r#"["",0]"#),
            Some(Loaded {
                title: String::new(),
                status: None
            }),
            "an error page has no title and no status"
        );
        assert_eq!(
            answer(r#"["t",null]"#),
            Some(Loaded {
                title: "t".to_string(),
                status: None
            })
        );
        assert_eq!(answer(r#""just a title""#), None);
        assert_eq!(loaded(&json(r#"{"exceptionDetails":{}}"#)), None);
    }

    #[test]
    fn a_status_worth_a_word_has_one() {
        assert_eq!(status_phrase(404), "404 not found");
        assert_eq!(status_phrase(503), "503 unavailable");
        assert_eq!(status_phrase(500), "500 server error");
        assert_eq!(status_phrase(599), "599");
        assert_eq!(sentence(&Problem::Status(403)), "403 forbidden");
    }
}
