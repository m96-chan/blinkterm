//! What this program calls itself to a page: the user agent, the client
//! hints behind it, and the languages.
//!
//! # Why the engine's own answer will not do
//!
//! A headless Chromium calls itself `HeadlessChrome/153.0.8010.52`, and its
//! client hints say `HeadlessChrome` too. Both are read as "a crawler", and
//! both are beside the point here: a person is at a terminal driving this,
//! and what is doing the rendering is Chromium, the same Chromium a desktop
//! Chrome is. The headless token describes how the engine was started, not
//! who is asking for the page.
//!
//! So the engine's own string is taken and two things are done to it. The
//! headless token becomes the ordinary one — `Chrome/153.0.0.0`, the reduced
//! form a current Chrome sends, rather than the build number a headless one
//! leaks. And `blinkterm/<version>` is appended, which is what every browser
//! built on somebody else's engine does: Edge appends `Edg/`, and the base
//! string stays so that a page sniffing for Chromium still finds it.
//!
//! That token is the honest half. This is a terminal browser, it is not
//! Chrome, and a page that wants to know can read it.
//!
//! # Measured
//!
//! Against `bot.sannysoft.com` on macOS 26.6 with
//! `chrome-headless-shell` 153, four user agents — the engine's own, a plain
//! Chrome one, that plus `blinkterm/0.1`, and a bare `blinkterm/0.1
//! (Chromium 153; macOS)` — gave the same verdict on every row but the one
//! that echoes the string back. Against three sites behind a CDN
//! (`cloudflare.com`, `github.com`, `stackoverflow.com`) all four got the
//! same answer as each other, site for site, including the one challenge.
//! The user agent was not what any of those gates read, so naming ourselves
//! costs nothing; what they read is the rest of it, and
//! [`crate::engine::flags`] says what of that is and is not fixable.
//!
//! # The hints, which the switch could not reach
//!
//! `--user-agent=` rewrites `navigator.userAgent` and nothing else:
//! `navigator.userAgentData.brands` keeps saying `HeadlessChrome`, because
//! it is built from the engine's own version and no switch touches it.
//! `Network.setUserAgentOverride` carries `userAgentMetadata` beside the
//! string, so the two agree — measured, the brands come back
//! `Chromium/153 | blinkterm/0.1 | Not(A:Brand/24`.
//!
//! It is per session, which is why [`crate::app::prepare_session`] sends it
//! rather than the launch doing it once: a target attached later would
//! otherwise be the one page in the browser still calling itself headless.

use crate::json::Json;

/// What this program calls itself in a user agent and in the client hints.
pub const PRODUCT: &str = "blinkterm";

/// Its version, from the manifest, so the two never drift.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The GREASE brand a Chromium sends beside the real ones, to keep a page
/// from matching the list exactly. A fixed one is enough here: the point of
/// it is that the list is not a fingerprint to match on, not that it varies
/// between runs.
const GREASE: (&str, &str) = ("Not(A:Brand", "24");

/// What a page is told about who is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// `navigator.userAgent`, and the `User-Agent` header.
    pub user_agent: String,
    /// `navigator.languages`, and the `Accept-Language` header. A plain
    /// comma-separated list: q-values written here come back in
    /// `navigator.languages` verbatim, `q=0.9` and all (measured).
    pub accept_language: String,
    /// The engine's major version, for the hints. `None` when the engine's
    /// string was not one this could read, in which case no hints are sent
    /// and only the string is overridden.
    pub chromium_major: Option<String>,
    /// Whether the page's console is listened to: `Runtime.enable` and
    /// `Log.enable` on every session, for [`crate::console`]. Here because
    /// it is what a page is told too — `Runtime` is how a page can find out
    /// it is heard, if only by the clock — and because this is what every
    /// session's preparation is already handed. `false` from [`Identity::new`];
    /// `app::boot` turns it on only when there is a recorder to take the
    /// events, since a session with `Runtime` on and nothing taking them is
    /// a mailbox full of console.
    pub console: bool,
}

impl Identity {
    /// From what the engine calls itself, the person's own `user-agent` if
    /// they wrote one, and their locale.
    ///
    /// An explicit `user-agent` is taken whole and nothing is appended: a
    /// person who wrote one wrote all of it, and a `blinkterm/…` stapled to
    /// the end would be this program contradicting them.
    pub fn new(engine_agent: Option<&str>, override_agent: Option<&str>, locale: &str) -> Identity {
        let accept_language = languages(locale);
        if let Some(agent) = override_agent {
            return Identity {
                user_agent: agent.to_string(),
                accept_language,
                chromium_major: None,
                console: false,
            };
        }
        let engine = engine_agent.unwrap_or_default();
        let major = major_version(engine);
        let user_agent = match &major {
            Some(major) => format!("{} {PRODUCT}/{VERSION}", deheadless(engine, major)),
            // An engine whose string this could not read is left alone but
            // still named: the token is the part that is ours to say.
            None if engine.is_empty() => String::new(),
            None => format!("{engine} {PRODUCT}/{VERSION}"),
        };
        Identity {
            user_agent,
            accept_language,
            chromium_major: major,
            console: false,
        }
    }

    /// The same, with the console listened to or not. See
    /// [`Identity::console`].
    pub fn with_console(self, console: bool) -> Identity {
        Identity { console, ..self }
    }

    /// The languages the person asked the engine for with an
    /// `--engine-arg --accept-lang=…`, over the locale's.
    ///
    /// The override's `acceptLanguage` replaces what the engine was started
    /// with — measured, `--accept-lang=fr` reads `fr` in `navigator.languages`
    /// until the override is sent and the locale's list after it — so a
    /// person's own switch has to be carried into the override, or the
    /// README's way of choosing a language quietly stops working.
    pub fn with_accept_language(self, explicit: Option<&str>) -> Identity {
        match explicit {
            Some(languages) => Identity {
                accept_language: languages.to_string(),
                ..self
            },
            None => self,
        }
    }

    /// The `Network.setUserAgentOverride` this is, or `None` when there is
    /// nothing to say — an engine that named itself in a way this could not
    /// read, and no `user-agent` of the person's own.
    pub fn command(&self) -> Option<(&'static str, Json)> {
        if self.user_agent.is_empty() {
            return None;
        }
        let mut fields = vec![
            ("userAgent", Json::string(&self.user_agent)),
            ("acceptLanguage", Json::string(&self.accept_language)),
        ];
        if let Some(major) = &self.chromium_major {
            fields.push(("userAgentMetadata", self.metadata(major)));
        }
        Some(("Network.setUserAgentOverride", Json::object(fields)))
    }

    /// The client hints, which say the same as the string.
    fn metadata(&self, major: &str) -> Json {
        let brand = |name: &str, version: &str| {
            Json::object(vec![
                ("brand", Json::string(name)),
                ("version", Json::string(version)),
            ])
        };
        Json::object(vec![
            (
                "brands",
                Json::Array(vec![
                    brand("Chromium", major),
                    brand(PRODUCT, VERSION),
                    brand(GREASE.0, GREASE.1),
                ]),
            ),
            (
                "fullVersionList",
                Json::Array(vec![
                    brand("Chromium", &format!("{major}.0.0.0")),
                    brand(PRODUCT, VERSION),
                    brand(GREASE.0, GREASE.1),
                ]),
            ),
            ("platform", Json::string(PLATFORM)),
            ("platformVersion", Json::string("")),
            ("architecture", Json::string(ARCHITECTURE)),
            ("model", Json::string("")),
            ("mobile", Json::Bool(false)),
            ("fullVersion", Json::string(format!("{major}.0.0.0"))),
        ])
    }
}

/// What the hints call this platform, spelled as Chromium spells it.
#[cfg(target_os = "macos")]
const PLATFORM: &str = "macOS";
#[cfg(target_os = "linux")]
const PLATFORM: &str = "Linux";
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const PLATFORM: &str = "Unknown";

/// The same for the instruction set. Chromium says `arm` and `x86`, not the
/// target triple's words.
#[cfg(target_arch = "aarch64")]
const ARCHITECTURE: &str = "arm";
#[cfg(not(target_arch = "aarch64"))]
const ARCHITECTURE: &str = "x86";

/// The engine's major version, out of `HeadlessChrome/153.0.8010.52` or
/// `Chrome/153.0.0.0`. `None` for a string with neither.
fn major_version(agent: &str) -> Option<String> {
    let at = agent
        .find("HeadlessChrome/")
        .map(|i| i + "HeadlessChrome/".len())
        .or_else(|| agent.find("Chrome/").map(|i| i + "Chrome/".len()))?;
    let digits: String = agent[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!digits.is_empty()).then_some(digits)
}

/// `HeadlessChrome/153.0.8010.52` becomes `Chrome/153.0.0.0`: the token an
/// ordinary Chromium sends, and the reduced version it sends it with, so
/// that the build number a headless engine leaks is not a fingerprint of
/// its own.
fn deheadless(agent: &str, major: &str) -> String {
    let reduced = format!("Chrome/{major}.0.0.0");
    if let Some(start) = agent.find("HeadlessChrome/") {
        let rest = &agent[start + "HeadlessChrome/".len()..];
        let end = rest
            .find(' ')
            .map_or(agent.len(), |i| start + "HeadlessChrome/".len() + i);
        return format!("{}{reduced}{}", &agent[..start], &agent[end..]);
    }
    if let Some(start) = agent.find("Chrome/") {
        let rest = &agent[start + "Chrome/".len()..];
        let end = rest
            .find(' ')
            .map_or(agent.len(), |i| start + "Chrome/".len() + i);
        return format!("{}{reduced}{}", &agent[..start], &agent[end..]);
    }
    agent.to_string()
}

/// The locale as a page wants its languages: `ja_JP.UTF-8` becomes
/// `ja-JP,ja,en`, and anything unset or `C` becomes `en-US,en`.
///
/// English is on the end of every list because a page with nothing in the
/// person's language should be served rather than refused, which is what a
/// desktop browser's list does too.
pub fn languages(locale: &str) -> String {
    let cut = locale.split('.').next().unwrap_or("");
    let cut = cut.split('@').next().unwrap_or("");
    if cut.is_empty() || cut == "C" || cut == "POSIX" {
        return "en-US,en".to_string();
    }
    let tag = cut.replace('_', "-");
    let language = tag.split('-').next().unwrap_or(&tag).to_string();
    let mut out = vec![tag.clone()];
    if language != tag {
        out.push(language.clone());
    }
    if language != "en" {
        out.push("en".to_string());
    }
    out.join(",")
}

/// The value of the last `--accept-lang=` among the engine's extra
/// arguments, the one the engine itself would take. An empty one is none.
pub fn accept_lang_arg(args: &[String]) -> Option<&str> {
    args.iter()
        .rev()
        .find_map(|arg| arg.strip_prefix("--accept-lang="))
        .filter(|languages| !languages.is_empty())
}

/// The person's locale, from the environment the way every POSIX program
/// reads it: `LC_ALL`, then `LC_MESSAGES`, then `LANG`.
pub fn locale() -> String {
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(value) = std::env::var_os(name) {
            let value = value.to_string_lossy().to_string();
            if !value.is_empty() {
                return value;
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADLESS: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                            (KHTML, like Gecko) HeadlessChrome/153.0.8010.52 Safari/537.36";

    /// The headless token goes, the version is the reduced one, and the name
    /// of this program is on the end.
    #[test]
    fn the_agent_is_the_engines_with_the_headless_token_out_and_ours_on() {
        let id = Identity::new(Some(HEADLESS), None, "ja_JP.UTF-8");
        assert_eq!(
            id.user_agent,
            format!(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36 blinkterm/{VERSION}"
            )
        );
        assert!(!id.user_agent.contains("Headless"), "{}", id.user_agent);
        // The build number a headless engine leaks is gone with it.
        assert!(!id.user_agent.contains("8010"), "{}", id.user_agent);
        assert_eq!(id.chromium_major.as_deref(), Some("153"));
    }

    /// A person who wrote a user agent wrote all of it.
    #[test]
    fn an_agent_of_the_persons_own_is_taken_whole() {
        let id = Identity::new(Some(HEADLESS), Some("my-crawler/2"), "en_US.UTF-8");
        assert_eq!(id.user_agent, "my-crawler/2");
        assert_eq!(id.chromium_major, None);
        // And no hints are sent, so that they cannot contradict it.
        let (_, params) = id.command().expect("a command");
        assert!(params.get("userAgentMetadata").is_none(), "{params:?}");
    }

    /// The hints say what the string says.
    #[test]
    fn the_hints_name_chromium_and_this_program_and_nothing_headless() {
        let id = Identity::new(Some(HEADLESS), None, "ja_JP.UTF-8");
        let (method, params) = id.command().expect("a command");
        assert_eq!(method, "Network.setUserAgentOverride");
        let brands = params
            .path(&["userAgentMetadata", "brands"])
            .and_then(Json::as_array)
            .expect("the brands");
        let names: Vec<&str> = brands
            .iter()
            .filter_map(|b| b.get("brand").and_then(Json::as_str))
            .collect();
        assert_eq!(names, vec!["Chromium", PRODUCT, GREASE.0]);
        assert_eq!(
            params
                .path(&["userAgentMetadata", "platform"])
                .and_then(Json::as_str),
            Some(PLATFORM)
        );
    }

    /// An engine that named itself in a way this cannot read is not guessed
    /// at, and one that said nothing at all is not overridden.
    #[test]
    fn an_unreadable_engine_is_named_but_not_invented_and_silence_says_nothing() {
        let odd = Identity::new(Some("SomeEngine/1"), None, "C");
        assert_eq!(odd.user_agent, format!("SomeEngine/1 {PRODUCT}/{VERSION}"));
        assert_eq!(odd.chromium_major, None);

        let silent = Identity::new(None, None, "C");
        assert_eq!(silent.user_agent, "");
        assert_eq!(silent.command(), None, "nothing to say, so nothing is sent");
    }

    /// The locale becomes the list a page reads, English last.
    /// An `--accept-lang=` the person passed wins over the locale, the last
    /// one when there are several, as it does in the engine.
    #[test]
    fn an_accept_lang_of_the_persons_own_wins_over_the_locale() {
        let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert_eq!(accept_lang_arg(&args(&["--accept-lang=ja"])), Some("ja"));
        assert_eq!(
            accept_lang_arg(&args(&[
                "--accept-lang=fr",
                "--mute-audio",
                "--accept-lang=de,en"
            ])),
            Some("de,en")
        );
        assert_eq!(accept_lang_arg(&args(&["--lang=ja"])), None);
        assert_eq!(accept_lang_arg(&args(&["--accept-lang="])), None);
        assert_eq!(accept_lang_arg(&[]), None);

        let id = Identity::new(Some(HEADLESS), None, "ja_JP.UTF-8");
        assert_eq!(id.clone().with_accept_language(None), id);
        let fr = id.with_accept_language(Some("fr"));
        assert_eq!(fr.accept_language, "fr");
        let (_, params) = fr.command().expect("an override");
        assert_eq!(
            params.get("acceptLanguage").and_then(Json::as_str),
            Some("fr")
        );
    }

    #[test]
    fn the_locale_becomes_a_language_list_with_english_on_the_end() {
        assert_eq!(languages("ja_JP.UTF-8"), "ja-JP,ja,en");
        assert_eq!(languages("en_US.UTF-8"), "en-US,en");
        assert_eq!(languages("de_DE@euro"), "de-DE,de,en");
        assert_eq!(languages("ja"), "ja,en");
        // Nothing, or the locale that means "no locale", is English.
        assert_eq!(languages(""), "en-US,en");
        assert_eq!(languages("C"), "en-US,en");
        assert_eq!(languages("POSIX"), "en-US,en");
    }

    /// No q-values: written here they come back in `navigator.languages`
    /// verbatim, `q=0.9` and all, which is not a language.
    #[test]
    fn the_language_list_carries_no_quality_values() {
        for locale in ["ja_JP.UTF-8", "en_US.UTF-8", "C"] {
            let list = languages(locale);
            assert!(!list.contains('q'), "{list}");
            assert!(!list.contains(';'), "{list}");
        }
    }
}
