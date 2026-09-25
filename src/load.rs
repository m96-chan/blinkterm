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
//! # What the engine says about trust, and what it does not
//!
//! Measured against `chrome-headless-shell` 153 with `Page.enable` and nothing
//! else, over plain and TLS servers on loopback and on a named host mapped to
//! loopback, a self-signed certificate refused and accepted, a page with real
//! mixed content, `data:`, `about:blank`, `file:` and a closed port: the
//! `Security` domain has nothing to add. `Security.enable` answers at once and
//! then `Security.visibleSecurityStateChanged` never fires, nor the deprecated
//! `securityStateChanged`; the one event it does send is
//! `Security.certificateError`, whose code is the same `ERR_CERT_*` the
//! `Page.navigate` reply already carries and [`reason`] already words. It is
//! not enabled.
//!
//! What is free is a field of the `Page.frameNavigated` that [`landing`]
//! already reads, `frame.secureContextType`:
//!
//! ```text
//! http://127.0.0.1:…/          SecureLocalhost
//! http://insecure.test:…/      InsecureScheme
//! https://127.0.0.1:…/         SecureLocalhost
//! https://secure.test:…/       Secure
//! data:text/html,…             InsecureScheme
//! about:blank                  InsecureScheme
//! file:///etc/hostname         Secure
//! the error page               InsecureScheme
//! ```
//!
//! So [`trust`] marks a page only when both agree: the scheme is `http` (or
//! `ws`) and the engine says `InsecureScheme`. Loopback is left unmarked, as a
//! desktop browser leaves it; `data:` and `about:` are not marked because
//! there is no server whose transport could have been secure; and a
//! certificate error is already a [`Problem::Unreachable`] with its reason.
//!
//! Mixed content is the one thing left, and the only place the engine says it
//! is the `Log` domain: two `Log.entryAdded` with `source: "security"` and a
//! text starting `Mixed Content:` per resource (Chromium now upgrades mixed
//! images to `https` and blocks them if that fails, except on an IP-address
//! host). `Log` is cheap — no entries at all for console output, one per
//! failed subresource — and a `Trust::Mixed` set from it, cleared by the next
//! landing, would be a third variant here. It is not built, because the
//! engine test for it needs an `https` page and this crate's tests serve with
//! `std`, which has no TLS: a parser tested and an enabling untested is the
//! wrong way round.
//!
//! # What a load says about itself
//!
//! A navigation announces itself before any byte comes back:
//! `Page.frameStartedNavigating`, with the url, three milliseconds after a
//! typed `Page.navigate` and four after a click on a link — and for a server
//! that never answers, that is the last thing said until somebody stops it.
//! `Page.frameStartedLoading` comes beside it with no url, and
//! `Page.frameStoppedLoading` ends every load however it ends: in the same
//! millisecond as the load event, after `Page.stopLoading`, and a millisecond
//! after a `pushState`, which sends the pair and nothing else. Every one of
//! them names a frame, and an iframe's load sends the same sequence under its
//! own id, so the main frame's id is read off `Page.getFrameTree` when a tab is
//! attached ([`main_frame`]) and off every main-frame landing since
//! ([`landed_frame`]).
//!
//! None of it is a fraction. `Page.setLifecycleEventsEnabled` adds a dozen
//! events a load, and its `networkAlmostIdle` fired at 795 ms of a 1512 ms
//! load and would fire at the same point of a thirty-second one; a percentage
//! would need the `Network` domain's byte counts, which the section above says
//! are not paid for. What is true, and free, is how long a load has been
//! going, which is what [`loading_hint`] says.
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
use crate::text;

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
/// a download — which [`crate::download::became_download`] tells apart.
/// Neither is anything the person needs telling about.
///
/// The code is the engine's and not the page's, but it is a string off the
/// pipe that [`reason`] prints when it does not know it, so it comes out as
/// plain text ([`crate::text::sanitize`]) like everything else read here.
pub fn failed(reply: &Json) -> Option<String> {
    let code = reply.get("errorText").and_then(Json::as_str)?;
    if code.is_empty() || code == "net::ERR_ABORTED" {
        return None;
    }
    Some(text::sanitize(code).into_owned())
}

/// Where a `Page.frameNavigated` says the main frame went.
///
/// `None` for a frame with a parent — an iframe navigating is an advert
/// changing, not the page — and for anything that is not the shape the engine
/// sends. The error page's own url, `chrome-error://chromewebdata/`, is never
/// what comes out: when `unreachableUrl` is there, it is the address.
///
/// Either url is plain text by the time it is a [`Landing`]: a page steers
/// where its frame goes, and whatever the engine's spelling of that turns out
/// to be, it is going on the row ([`crate::text::sanitize`]).
pub fn landing(params: &Json) -> Option<Landing> {
    let frame = params.get("frame")?;
    if frame.get("parentId").is_some() {
        return None;
    }
    if let Some(unreachable) = frame.get("unreachableUrl").and_then(Json::as_str) {
        if !unreachable.is_empty() {
            return Some(Landing::Unreachable(
                text::sanitize(unreachable).into_owned(),
            ));
        }
    }
    let url = frame.get("url").and_then(Json::as_str)?;
    Some(Landing::Document(text::sanitize(url).into_owned()))
}

/// The url the tab is at, read off a `Page.getNavigationHistory` reply: the
/// entry at `currentIndex`. `None` when the reply is not that shape.
///
/// Here rather than where it is used because it is the one place a url comes
/// out of the history, and a url that comes out of anywhere is sanitized
/// where it is read ([`crate::text::sanitize`]).
pub fn current_url(history: &Json) -> Option<String> {
    let index = history.get("currentIndex").and_then(Json::as_i64)?;
    let url = history
        .get("entries")
        .and_then(Json::as_array)?
        .get(usize::try_from(index).ok()?)?
        .get("url")
        .and_then(Json::as_str)?;
    Some(text::sanitize(url).into_owned())
}

/// Whether the document's transport is one to warn about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Trust {
    /// Nothing known, or nothing to say: `https`, `file:`, `about:`, `data:`,
    /// loopback, an error page.
    #[default]
    Plain,
    /// `http://` on a host the engine calls an insecure scheme: anybody on
    /// the way could have read it or changed it.
    Insecure,
}

impl Trust {
    /// The words the row says for it, if any. ASCII and not a padlock: a
    /// glyph like that is ambiguous-width in East Asian terminals, and a row
    /// one cell out is a row that wraps.
    pub fn words(self) -> Option<&'static str> {
        match self {
            Trust::Plain => None,
            Trust::Insecure => Some("not secure"),
        }
    }
}

/// The trust of the document a `Page.frameNavigated` lands, from its url's
/// scheme and the engine's `secureContextType` together (the table is in the
/// module documentation).
///
/// `Plain` for an iframe, which is not the page; for an error page, whose
/// `InsecureScheme` is about `chrome-error:` and not about anything the
/// person asked for; and for every shape that is not the one measured — a
/// Chromium that renamed the field would un-mark pages rather than mark all
/// of them, and the engine test that asserts the field is there is what
/// notices.
pub fn trust(params: &Json) -> Trust {
    let Some(frame) = params.get("frame") else {
        return Trust::Plain;
    };
    if frame.get("parentId").is_some() {
        return Trust::Plain;
    }
    let unreachable = frame.get("unreachableUrl").and_then(Json::as_str);
    if unreachable.is_some_and(|url| !url.is_empty()) {
        return Trust::Plain;
    }
    let url = frame.get("url").and_then(Json::as_str).unwrap_or_default();
    let scheme = url.split_once(':').map_or("", |(scheme, _)| scheme);
    let plain = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("ws");
    let context = frame.get("secureContextType").and_then(Json::as_str);
    if plain && context == Some("InsecureScheme") {
        Trust::Insecure
    } else {
        Trust::Plain
    }
}

/// Where a `Page.frameStartedNavigating` says the main frame is going, if it
/// is the main frame.
///
/// `frame` is the tab's main frame's id, or `None` when the tab does not know
/// it yet — and then the departure is taken as the main frame's, because a
/// fresh tab's first navigation is. The url is the page's choice (a link's
/// href, a script's `location`) and is about to be on the row, so it comes
/// out as plain text ([`crate::text::sanitize`]).
///
/// `None` too for a navigation within the document — a fragment, a history
/// step between two `pushState`s — whose `navigationType` says so: nothing
/// is going to land for it (the engine says `navigatedWithinDocument`
/// instead of `frameNavigated`), so a "loading" said for it would be said
/// until the next real load.
pub fn started(params: &Json, frame: Option<&str>) -> Option<String> {
    if !is_main(params, frame) {
        return None;
    }
    let kind = params
        .get("navigationType")
        .and_then(Json::as_str)
        .unwrap_or_default();
    if kind.contains("sameDocument") || kind.contains("SameDocument") {
        return None;
    }
    let url = params.get("url").and_then(Json::as_str)?;
    Some(text::sanitize(url).into_owned())
}

/// Whether an event that names a frame by `frameId` is about the main frame:
/// the tab's own, or any at all while the tab does not know its own.
pub fn is_main(params: &Json, frame: Option<&str>) -> bool {
    match (frame_id(params), frame) {
        (Some(id), Some(main)) => id == main,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// The frame a `Page.frameStartedLoading`, `Page.frameStoppedLoading`,
/// `Page.frameStartedNavigating` or `Page.navigate` reply names.
pub fn frame_id(params: &Json) -> Option<&str> {
    params.get("frameId").and_then(Json::as_str)
}

/// The main frame's id out of a `Page.getFrameTree` reply.
pub fn main_frame(reply: &Json) -> Option<String> {
    reply
        .path(&["frameTree", "frame", "id"])
        .and_then(Json::as_str)
        .map(str::to_string)
}

/// The main frame's id out of a `Page.frameNavigated`, when it is the main
/// frame's. A main frame kept its id across every navigation measured, but
/// which process a frame lives in is the engine's business, so every landing
/// refreshes it rather than trusting the one read at attach.
pub fn landed_frame(params: &Json) -> Option<String> {
    let frame = params.get("frame")?;
    if frame.get("parentId").is_some() {
        return None;
    }
    frame.get("id").and_then(Json::as_str).map(str::to_string)
}

/// The right-hand words while a load is going: `esc stops`, and once it has
/// been going a second, for how long — `4s  esc stops`.
///
/// Seconds and not a percentage, because nothing free is a fraction (see the
/// module documentation); whole seconds counted up, because a person
/// compares them against their own patience, and a tenth of a second
/// changing would redraw the row ten times as often to say nothing more.
pub fn loading_hint(seconds: Option<u64>) -> String {
    match seconds {
        Some(seconds) => format!("{seconds}s  esc stops"),
        None => "esc stops".to_string(),
    }
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
    /// Plain text: [`crate::text::sanitize`] has been over it, so a title
    /// that was an escape sequence is its letters.
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
    let title = text::sanitize(value.first()?.as_str()?).into_owned();
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
    fn a_title_with_an_escape_in_it_is_read_as_its_letters() {
        // The issue's pair, as the engine would send them: a JSON `\u` escape
        // is how a control character arrives, and `json.rs` has made it the
        // character itself before anything here sees it.
        let answer = |value: &str| {
            loaded(&json(&format!(
                r#"{{"result":{{"type":"object","value":{value}}}}}"#
            )))
            .map(|loaded| loaded.title)
        };
        assert_eq!(
            answer(r#"["\u001b]0;x\u0007",0]"#),
            Some("]0;x".to_string())
        );
        assert_eq!(answer(r#"["a\rb",0]"#), Some("a b".to_string()));
    }

    #[test]
    fn a_landing_and_a_reason_are_plain_text() {
        let error_page = json(
            r#"{"frame":{"id":"F","url":"chrome-error://chromewebdata/",
                "unreachableUrl":"http://example.com/\u001b]0;x\u0007"}}"#,
        );
        assert_eq!(
            landing(&error_page),
            Some(Landing::Unreachable("http://example.com/]0;x".to_string()))
        );
        let document = json(r#"{"frame":{"id":"F","url":"https://example.com/\u001b[2J"}}"#);
        assert_eq!(
            landing(&document),
            Some(Landing::Document("https://example.com/[2J".to_string()))
        );
        assert_eq!(
            failed(&json(r#"{"errorText":"net::ERR_\u009b2JODD"}"#)),
            Some("net::ERR_2JODD".to_string())
        );
    }

    #[test]
    fn the_current_url_comes_out_of_the_history() {
        let history = json(
            r#"{"currentIndex":1,"entries":[
                {"id":1,"url":"about:blank","title":""},
                {"id":2,"url":"https://example.com/\u202emoc.knab","title":"x"}]}"#,
        );
        assert_eq!(
            current_url(&history),
            Some("https://example.com/moc.knab".to_string())
        );
        assert_eq!(
            current_url(&json(r#"{"currentIndex":5,"entries":[]}"#)),
            None,
            "an index past the end is no url"
        );
        assert_eq!(
            current_url(&json(r#"{"currentIndex":-1,"entries":[]}"#)),
            None
        );
        assert_eq!(current_url(&json(r#"{"entries":[{"url":"x"}]}"#)), None);
    }

    /// A `Page.frameNavigated` for the main frame, in the shape the engine
    /// sent for each document in the module documentation's table.
    fn navigated(url: &str, context: &str, unreachable: Option<&str>) -> Json {
        let unreachable = unreachable
            .map(|url| format!(r#""unreachableUrl":"{url}","#))
            .unwrap_or_default();
        json(&format!(
            r#"{{"frame":{{"id":"31FA","loaderId":"5B26","url":"{url}","domainAndRegistry":"",
                "securityOrigin":"x","securityOriginDetails":{{"isLocalhost":false}},
                "mimeType":"text/html",{unreachable}"adFrameStatus":{{"adFrameType":"none"}},
                "secureContextType":"{context}","crossOriginIsolatedContextType":"NotIsolated",
                "gatedAPIFeatures":[]}},"type":"Navigation"}}"#
        ))
    }

    #[test]
    fn a_plain_http_page_on_a_named_host_is_not_secure_and_the_rest_are_plain() {
        let measured = [
            (
                "http://127.0.0.1:34189/",
                "SecureLocalhost",
                None,
                Trust::Plain,
            ),
            (
                "http://insecure.test:37589/",
                "InsecureScheme",
                None,
                Trust::Insecure,
            ),
            (
                "https://127.0.0.1:33805/",
                "SecureLocalhost",
                None,
                Trust::Plain,
            ),
            ("https://secure.test:43745/", "Secure", None, Trust::Plain),
            (
                "data:text/html,<title>d</title>",
                "InsecureScheme",
                None,
                Trust::Plain,
            ),
            ("about:blank", "InsecureScheme", None, Trust::Plain),
            ("file:///etc/hostname", "Secure", None, Trust::Plain),
            (
                "chrome-error://chromewebdata/",
                "InsecureScheme",
                Some("http://insecure.test:33767/"),
                Trust::Plain,
            ),
        ];
        for (url, context, unreachable, wanted) in measured {
            assert_eq!(
                trust(&navigated(url, context, unreachable)),
                wanted,
                "{url} {context}"
            );
        }
        // A scheme is read without regard to case, as a url's is.
        assert_eq!(
            trust(&navigated("HTTP://wiki.corp/", "InsecureScheme", None)),
            Trust::Insecure
        );
        // An iframe on plain http is not the page.
        let iframe = json(
            r#"{"frame":{"id":"9","parentId":"31FA","url":"http://ads.example/",
                "secureContextType":"InsecureScheme"}}"#,
        );
        assert_eq!(trust(&iframe), Trust::Plain);
        assert_eq!(landed_frame(&iframe), None);
        // And a frame without the field is nothing to say, not a warning.
        assert_eq!(
            trust(&json(r#"{"frame":{"id":"1","url":"http://wiki.corp/"}}"#)),
            Trust::Plain
        );
        assert_eq!(Trust::Insecure.words(), Some("not secure"));
        assert_eq!(Trust::Plain.words(), None);
        assert_eq!(
            landed_frame(&navigated("about:blank", "InsecureScheme", None)),
            Some("31FA".to_string())
        );
    }

    #[test]
    fn the_main_frames_departure_names_where_it_is_going() {
        // Exactly what the engine sent for a typed navigation to a server that
        // never answered.
        let departure = json(
            r#"{"frameId":"55101129AF7BCC483B7F40316B8D789D","url":"http://127.0.0.1:36913/hang",
                "loaderId":"F850C8E9C75CF1C284B1AEE143EEE8DC","navigationType":"differentDocument"}"#,
        );
        assert_eq!(
            started(&departure, Some("55101129AF7BCC483B7F40316B8D789D")),
            Some("http://127.0.0.1:36913/hang".to_string())
        );
        assert_eq!(
            started(&departure, Some("C98A215D9979FB477396B8A1716121E1")),
            None,
            "an iframe leaving is not the page leaving"
        );
        assert_eq!(
            started(&departure, None),
            Some("http://127.0.0.1:36913/hang".to_string()),
            "a tab that does not know its frame yet takes the first it hears"
        );
        let hostile =
            json(r#"{"frameId":"F","url":"http://evil.example/\u202emoc.knab\u001b]0;x\u0007"}"#);
        assert_eq!(
            started(&hostile, Some("F")),
            Some("http://evil.example/moc.knab]0;x".to_string())
        );
        for within in ["sameDocument", "historySameDocument"] {
            let fragment = json(&format!(
                r#"{{"frameId":"F","url":"http://a.example/#top","navigationType":"{within}"}}"#
            ));
            assert_eq!(started(&fragment, Some("F")), None, "{within}");
        }
        assert_eq!(frame_id(&json(r#"{"frameId":"F"}"#)), Some("F"));
        assert_eq!(frame_id(&json(r#"{"frame":"F"}"#)), None);
        assert!(!is_main(&json("{}"), None), "no frame named is no frame");
    }

    #[test]
    fn the_frame_tree_names_the_main_frame() {
        let reply = json(
            r#"{"frameTree":{"frame":{"id":"A","loaderId":"L","url":"about:blank"},
                "childFrames":[{"frame":{"id":"B","parentId":"A","url":"x"}}]}}"#,
        );
        assert_eq!(main_frame(&reply), Some("A".to_string()));
        assert_eq!(main_frame(&json(r#"{"frameTree":{}}"#)), None);
    }

    #[test]
    fn the_loading_hint_says_esc_and_then_the_seconds() {
        assert_eq!(loading_hint(None), "esc stops");
        assert_eq!(loading_hint(Some(4)), "4s  esc stops");
        assert_eq!(loading_hint(Some(120)), "120s  esc stops");
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
