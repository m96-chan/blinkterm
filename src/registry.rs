//! The profiles a person has named, and which one a run is for.
//!
//! A profile is a directory ([`crate::profile`]), and until this module the
//! only way to have two of them was to type the second one's path, every
//! time, or keep a shell alias for it. Chrome's answer — a work profile, a
//! personal one, one for testing, chosen by name — is what this is: a short
//! list of names, each with its directory, one of them the default, kept in
//! `$XDG_DATA_HOME/blinkterm/profiles.json` (or
//! `~/.local/share/blinkterm/profiles.json`) beside the profile that was
//! always there.
//!
//! # The file
//!
//! ```json
//! {
//!   "version": 1,
//!   "default": "default",
//!   "profiles": [
//!     { "id": "default", "name": "Default", "dir": "profile", "created": 1759276800 },
//!     { "id": "3f9a1c0b7e2d", "name": "Work", "dir": "profiles/3f9a1c0b7e2d", "created": 1759280000 }
//!   ]
//! }
//! ```
//!
//! Each profile has an `id` that never changes, a `name` that is only ever
//! shown, and a `dir`. A relative `dir` is one this program made and looks
//! after — joined onto the data directory, so it moves with
//! `$XDG_DATA_HOME` — and an absolute one is a directory the person already
//! had and registered with `blinkterm profiles create <name> --dir <dir>`,
//! which `remove` forgets and never deletes. There is no separate flag for
//! which is which; the field is the rule. `default` is an `id`, or `null`.
//!
//! A name is never part of a path. A managed profile's directory is
//! `profiles/<id>`, where the id is twelve random hex characters, so a
//! rename is a change to one string in this file and nothing on disk moves,
//! and a name with a `..` or a `/` in it — refused anyway — could not reach
//! the filesystem if it got in. Every id is checked against
//! `[a-z0-9][a-z0-9-]{0,31}` and every relative `dir` against `..` and `.`
//! whenever the file is read, for the same reason: the file is the person's
//! to edit, and a typo in it must not point `remove` at the wrong directory.
//!
//! # The first start after upgrading
//!
//! The first run that needs the registry and finds none writes one with a
//! single entry, `Default`, whose `dir` is `profile` — the directory every
//! earlier blinkterm used. Nothing is moved or copied: the cookie jar, the
//! history and the saved tabs stay exactly where they were, an older
//! blinkterm run afterwards still finds them, and the `--remote` socket is
//! where `BROWSER='blinkterm --remote'` always looked for it.
//!
//! A file that cannot be read — not JSON, a duplicate name, an id with a `/`
//! in it — is refused with a sentence that names it, and never written over:
//! rewriting it would forget the profiles it lists, and their directories
//! would be left where nobody would find them again. A file from a newer
//! blinkterm (`version` above [`VERSION`]) is refused for the same reason.
//! `--profile <dir>` and `--temp-profile` never read it, so neither stops
//! working while it is being fixed.
//!
//! # Two at once
//!
//! Two `blinkterm profiles` commands, or a command and a picker, can change
//! the file at the same moment, and the rule that closes it is the
//! bookmarks' ([`crate::bookmarks`]): every change holds `profiles.lock`
//! ([`crate::profile::hold`], a blocking `flock` on a file that is never
//! renamed over), reads the file again under it, changes what it read, and
//! writes `profiles.json.tmp` renamed over `profiles.json`. A reader takes no
//! lock: a rename replaces the whole file, so it reads the old list or the
//! new one, never half of each. A profile's own `blinkterm.lock` is not this
//! lock, and is taken here only by [`remove`], which refuses a profile that
//! is in use.

use std::fs::OpenOptions;
use std::io::{IsTerminal, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json::Json;
use crate::profile::{self, Choice, Profile, Tried};
use crate::text;

/// The registry, in the data directory: `<data>/profiles.json`.
pub const FILE: &str = "profiles.json";

/// The lock beside it, never renamed over: see [`update`].
pub const LOCK: &str = "profiles.lock";

/// Where managed profiles are made: `<data>/profiles/<id>/`.
pub const MANAGED_DIR: &str = "profiles";

/// Where a removed managed profile goes: `<data>/trash/<id>-<unix seconds>/`.
pub const TRASH_DIR: &str = "trash";

/// The directory every blinkterm before the registry used, which the
/// migrated `Default` entry points at.
pub const LEGACY_DIR: &str = "profile";

/// The migrated entry's id, fixed so that it reads the same in every
/// registry.
pub const DEFAULT_ID: &str = "default";

/// The version of the file this blinkterm writes, and the newest it reads.
pub const VERSION: u32 = 1;

/// The longest name, in characters: a word on the status row, not a
/// sentence.
pub const MAX_NAME_CHARS: usize = 64;

/// The biggest file read: a list of names is a few hundred bytes, and
/// anything near this is not one.
pub const MAX_REGISTRY_BYTES: usize = 256 * 1024;

/// One profile in the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Stable, never shown, never changed: `[a-z0-9][a-z0-9-]{0,31}`.
    pub id: String,
    /// What the person calls it; unique without regard to case.
    pub name: String,
    /// Relative to the data directory for a managed profile, absolute for
    /// one the person registered.
    pub dir: PathBuf,
    /// When it was made, in Unix seconds; only for the person reading the
    /// file.
    pub created: u64,
}

impl Entry {
    /// Whether this program made the directory and so may move it to the
    /// trash: a relative `dir`.
    pub fn is_managed(&self) -> bool {
        self.dir.is_relative()
    }
}

/// The registry as read: the profiles in file order, which is the order
/// they were made in, and which one is the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    /// The data directory it was read from, which relative `dir`s are
    /// joined onto.
    data: PathBuf,
    /// An [`Entry::id`], or `None` when no profile is the default.
    pub default: Option<String>,
    pub profiles: Vec<Entry>,
}

/// What [`remove`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removed {
    /// A managed profile, moved to `to` under the trash directory; `None`
    /// when its directory had never been made, and there was nothing to
    /// move.
    Trashed {
        name: String,
        to: Option<PathBuf>,
        was_default: bool,
    },
    /// A registered directory, forgotten and left where it is.
    Forgotten {
        name: String,
        dir: PathBuf,
        was_default: bool,
    },
}

/// Whether [`select`] may ask on the terminal when nothing chose a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// When stdin and stdout are both a terminal.
    IfTerminal,
    /// Never, for the reason given, which goes into the sentence that says
    /// no profile was chosen.
    Never(&'static str),
}

/// A profile chosen: what [`select`] resolves every [`Choice`] to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    /// The directory, absolute; `None` for a temporary profile.
    pub dir: Option<PathBuf>,
    /// The registry's id for it, for a named profile.
    pub id: Option<String>,
    /// The name for the status row: only for a registry profile, and only
    /// while the registry holds two or more, since one profile needs no
    /// telling apart and somebody who never made a second sees no change.
    pub label: Option<String>,
}

impl Registry {
    /// `<data>/profiles.json`.
    pub fn path(data: &Path) -> PathBuf {
        data.join(FILE)
    }

    /// `<data>/profiles.lock`.
    pub fn lock_path(data: &Path) -> PathBuf {
        data.join(LOCK)
    }

    /// What a data directory with no registry has: `Default`, at the
    /// directory every earlier blinkterm used, and the default.
    pub fn initial(data: &Path) -> Registry {
        Registry {
            data: data.to_path_buf(),
            default: Some(DEFAULT_ID.to_string()),
            profiles: vec![Entry {
                id: DEFAULT_ID.to_string(),
                name: "Default".to_string(),
                dir: PathBuf::from(LEGACY_DIR),
                created: now_secs(),
            }],
        }
    }

    /// The data directory this registry is in.
    pub fn data(&self) -> &Path {
        &self.data
    }

    /// The file's text, read: every check a hand-edited file needs is here,
    /// and any one failing is the sentence that names the file and the way
    /// round it. Keys this version does not know are passed over.
    pub fn parse(data: &Path, text: &str) -> Result<Registry, String> {
        let path = Registry::path(data);
        let corrupt = |why: String| {
            format!(
                "cannot read {}: {why}; fix it or move it aside \
                 (blinkterm --profile <dir> works meanwhile)",
                path.display()
            )
        };
        if text.len() > MAX_REGISTRY_BYTES {
            return Err(corrupt(format!(
                "{} bytes is too big; the limit is {} KiB",
                text.len(),
                MAX_REGISTRY_BYTES / 1024
            )));
        }
        let json = Json::parse(text).map_err(|e| corrupt(format!("not JSON: {e}")))?;
        if !matches!(json, Json::Object(_)) {
            return Err(corrupt("it is not a JSON object".to_string()));
        }
        let version = json
            .get("version")
            .and_then(Json::as_f64)
            .filter(|v| v.fract() == 0.0 && *v >= 1.0)
            .ok_or_else(|| corrupt("it has no version".to_string()))?;
        if version > f64::from(VERSION) {
            return Err(format!(
                "{} was written by a newer blinkterm (version {version}; this one reads \
                 {VERSION}); upgrade it, or use --profile <dir>",
                path.display()
            ));
        }
        let items = json
            .get("profiles")
            .and_then(Json::as_array)
            .ok_or_else(|| corrupt("profiles is not a list".to_string()))?;
        let mut profiles: Vec<Entry> = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let at = |why: &str| corrupt(format!("profile {} {why}", index + 1));
            let field = |key: &str| item.get(key).and_then(Json::as_str);
            let id = field("id").ok_or_else(|| at("has no id"))?;
            if !valid_id(id) {
                return Err(at(&format!(
                    "has the id {:?}, which is not a-z, 0-9 and -",
                    text::sanitize(id)
                )));
            }
            let name = field("name").ok_or_else(|| at("has no name"))?;
            validate_name(name).map_err(|why| at(&format!("has a bad name: {why}")))?;
            let dir = field("dir").ok_or_else(|| at("has no dir"))?;
            let dir = PathBuf::from(dir);
            if dir.as_os_str().is_empty() {
                return Err(at("has an empty dir"));
            }
            if dir.is_relative() && !dir.components().all(|c| matches!(c, Component::Normal(_))) {
                return Err(at("has a relative dir with . or .. in it"));
            }
            let created = match item.get("created") {
                None | Some(Json::Null) => 0,
                Some(value) => value
                    .as_f64()
                    .filter(|n| n.fract() == 0.0 && *n >= 0.0)
                    .ok_or_else(|| at("has a created that is not a time"))?
                    as u64,
            };
            if let Some(other) = profiles.iter().find(|e| e.id == id) {
                return Err(corrupt(format!("two profiles have the id {:?}", other.id)));
            }
            if let Some(other) = profiles
                .iter()
                .find(|e| e.name.to_lowercase() == name.to_lowercase())
            {
                return Err(corrupt(format!(
                    "two profiles are named {:?}",
                    text::sanitize(&other.name)
                )));
            }
            profiles.push(Entry {
                id: id.to_string(),
                name: name.to_string(),
                dir,
                created,
            });
        }
        let default = match json.get("default") {
            None | Some(Json::Null) => None,
            Some(Json::String(id)) if profiles.iter().any(|e| &e.id == id) => Some(id.clone()),
            Some(_) => {
                return Err(corrupt(
                    "default is not the id of a profile in it".to_string(),
                ))
            }
        };
        Ok(Registry {
            data: data.to_path_buf(),
            default,
            profiles,
        })
    }

    /// The file's text: one profile a line, two spaces in, so that it reads
    /// and diffs as the list it is. [`Registry::parse`] of this is `self`.
    pub fn render(&self) -> String {
        let string = |s: &str| Json::String(s.to_string()).to_string();
        let mut out = format!("{{\n  \"version\": {VERSION},\n  \"default\": ");
        match &self.default {
            Some(id) => out.push_str(&string(id)),
            None => out.push_str("null"),
        }
        out.push_str(",\n  \"profiles\": [");
        for (index, entry) in self.profiles.iter().enumerate() {
            out.push_str(if index == 0 { "\n" } else { ",\n" });
            out.push_str(&format!(
                "    {{ \"id\": {}, \"name\": {}, \"dir\": {}, \"created\": {} }}",
                string(&entry.id),
                string(&entry.name),
                string(&entry.dir.to_string_lossy()),
                entry.created
            ));
        }
        if !self.profiles.is_empty() {
            out.push_str("\n  ");
        }
        out.push_str("]\n}\n");
        out
    }

    /// The registry in `data`, read now without the lock; `None` when there
    /// is no file.
    pub fn load(data: &Path) -> Result<Option<Registry>, String> {
        let path = Registry::path(data);
        let mut bytes = Vec::new();
        match std::fs::File::open(&path) {
            Ok(file) => {
                // One byte past the limit is enough to know it is over it.
                file.take(MAX_REGISTRY_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        }
        let text = String::from_utf8(bytes).map_err(|_| {
            format!(
                "cannot read {}: it is not UTF-8 text; fix it or move it aside \
                 (blinkterm --profile <dir> works meanwhile)",
                path.display()
            )
        })?;
        Registry::parse(data, &text).map(Some)
    }

    /// The registry in `data`, written first if there is none: the
    /// migration, which registers the profile every earlier blinkterm used
    /// as `Default` and moves nothing. An existing file is read and not
    /// written.
    pub fn open(data: &Path) -> Result<Registry, String> {
        if let Some(registry) = Registry::load(data)? {
            return Ok(registry);
        }
        let held = profile::hold(&Registry::lock_path(data))?;
        // Somebody else may have written it while this waited for the lock.
        if let Some(registry) = Registry::load(data)? {
            return Ok(registry);
        }
        let registry = Registry::initial(data);
        write(data, &registry)?;
        drop(held);
        Ok(registry)
    }

    /// The profile named `name`, whatever the case it was typed in.
    pub fn by_name(&self, name: &str) -> Option<&Entry> {
        let wanted = name.to_lowercase();
        self.profiles
            .iter()
            .find(|e| e.name.to_lowercase() == wanted)
    }

    /// The profile with this id.
    pub fn by_id(&self, id: &str) -> Option<&Entry> {
        self.profiles.iter().find(|e| e.id == id)
    }

    /// The default profile, if one is.
    pub fn default_entry(&self) -> Option<&Entry> {
        self.default.as_deref().and_then(|id| self.by_id(id))
    }

    /// Where `entry`'s directory is: joined onto the data directory when it
    /// is managed, as it is when not.
    pub fn dir_of(&self, entry: &Entry) -> PathBuf {
        if entry.is_managed() {
            self.data.join(&entry.dir)
        } else {
            entry.dir.clone()
        }
    }

    /// `<data>/trash`.
    pub fn trash_dir(&self) -> PathBuf {
        self.data.join(TRASH_DIR)
    }

    /// `entry`, as a choice: its directory, its id, and its name when the
    /// row should show one.
    pub fn selected(&self, entry: &Entry) -> Selected {
        Selected {
            dir: Some(self.dir_of(entry)),
            id: Some(entry.id.clone()),
            label: (self.profiles.len() >= 2).then(|| entry.name.clone()),
        }
    }
}

/// Change the registry in `data`: hold [`LOCK`], read the file again — or
/// start from [`Registry::initial`] if there is none — apply `change`, and
/// write the result to `profiles.json.tmp` renamed over `profiles.json`.
///
/// Reading again under the lock is the point: a change is made to what is
/// there now, not to what this process read a moment ago, so a `profiles
/// create` in another terminal between two lines of the picker is kept
/// rather than written over. An `Err` from `change` writes nothing.
pub fn update<T>(
    data: &Path,
    change: impl FnOnce(&mut Registry) -> Result<T, String>,
) -> Result<T, String> {
    let held = profile::hold(&Registry::lock_path(data))?;
    let mut registry = match Registry::load(data)? {
        Some(registry) => registry,
        None => Registry::initial(data),
    };
    let answer = change(&mut registry)?;
    write(data, &registry)?;
    drop(held);
    Ok(answer)
}

/// The registry written over the file, whole: to `profiles.json.tmp`, 0600,
/// renamed over it. The caller holds the lock.
fn write(data: &Path, registry: &Registry) -> Result<(), String> {
    let path = Registry::path(data);
    let fresh = path.with_extension("json.tmp");
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&fresh)
        .and_then(|mut file| file.write_all(registry.render().as_bytes()))
        .and_then(|()| std::fs::rename(&fresh, &path))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Make a profile called `name`: a managed one under `profiles/<id>` when
/// `dir` is `None`, the directory made now; or the existing directory `dir`,
/// absolute, registered and not touched. The first profile in a registry
/// with no default becomes the default.
pub fn create(data: &Path, name: &str, dir: Option<PathBuf>) -> Result<Entry, String> {
    validate_name(name)?;
    let external = match dir {
        None => None,
        Some(dir) => {
            if !dir.is_absolute() {
                return Err(
                    "profiles create needs an absolute --dir, or none for a managed one"
                        .to_string(),
                );
            }
            let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
            if dir.to_str().is_none() {
                return Err(format!(
                    "{} is not UTF-8, so it cannot be written into {FILE}",
                    dir.display()
                ));
            }
            Some(dir)
        }
    };
    update(data, |registry| {
        if let Some(existing) = registry.by_name(name) {
            return Err(taken_name(&existing.name));
        }
        let dir = match external {
            Some(dir) => {
                if let Some(other) = registry.profiles.iter().find(|e| registry.dir_of(e) == dir) {
                    return Err(format!(
                        "{} is already the profile \"{}\"",
                        dir.display(),
                        text::sanitize(&other.name)
                    ));
                }
                dir
            }
            None => PathBuf::new(),
        };
        let id = new_id(&registry.profiles);
        let dir = if dir.as_os_str().is_empty() {
            let relative = Path::new(MANAGED_DIR).join(&id);
            // Before the entry is written, so that it never names a
            // directory that could not be made.
            profile::make_private_dir(&data.join(&relative))?;
            relative
        } else {
            dir
        };
        let entry = Entry {
            id,
            name: name.to_string(),
            dir,
            created: now_secs(),
        };
        if registry.default_entry().is_none() {
            registry.default = Some(entry.id.clone());
        }
        registry.profiles.push(entry.clone());
        Ok(entry)
    })
}

/// Call the profile `from` by the name `to` instead. Only the name changes:
/// the id and the directory are as they were.
pub fn rename(data: &Path, from: &str, to: &str) -> Result<Entry, String> {
    validate_name(to)?;
    update(data, |registry| {
        let id = registry
            .by_name(from)
            .ok_or_else(|| unknown_name(from))?
            .id
            .clone();
        if let Some(other) = registry.by_name(to).filter(|e| e.id != id) {
            return Err(taken_name(&other.name));
        }
        let entry = registry
            .profiles
            .iter_mut()
            .find(|e| e.id == id)
            .expect("the entry by_name found");
        entry.name = to.to_string();
        Ok(entry.clone())
    })
}

/// Make `name` the profile a start with nothing chosen opens.
pub fn set_default(data: &Path, name: &str) -> Result<Entry, String> {
    update(data, |registry| {
        let entry = registry
            .by_name(name)
            .ok_or_else(|| unknown_name(name))?
            .clone();
        registry.default = Some(entry.id.clone());
        Ok(entry)
    })
}

/// Take `name` out of the registry: a managed profile's directory moved to
/// `<data>/trash/<id>-<unix seconds>`, where its cookies and logins wait
/// until the person deletes them; a registered directory forgotten and left
/// alone. Refused, with the registry untouched, while a blinkterm is using
/// the profile — its own lock is tried, without waiting, under the
/// registry's. Removing the default leaves no default rather than picking
/// another, so that a start that had one never quietly opens a different
/// identity.
///
/// There is one narrow race left open: a start that read the registry just
/// before the directory moved, and has not yet taken the profile's lock,
/// makes an empty directory at the old path. That window is the time
/// between a read and an `flock`; what was removed is in the trash as asked.
pub fn remove(data: &Path, name: &str) -> Result<Removed, String> {
    let (removed, held) = update(data, |registry| {
        let entry = registry
            .by_name(name)
            .ok_or_else(|| unknown_name(name))?
            .clone();
        let dir = registry.dir_of(&entry);
        let held = if dir.is_dir() {
            match profile::try_lock(&dir)? {
                Tried::Taken(file) => Some(file),
                Tried::Held(pid) => {
                    let pid = pid.map(|pid| format!(" (pid {pid})")).unwrap_or_default();
                    return Err(format!(
                        "the profile \"{}\" is in use by another blinkterm{pid}; quit it first",
                        text::sanitize(&entry.name)
                    ));
                }
            }
        } else {
            None
        };
        let was_default = registry.default.as_deref() == Some(entry.id.as_str());
        let removed = if entry.is_managed() {
            let to = if held.is_some() {
                let trash = registry.trash_dir();
                profile::make_private_dir(&trash)?;
                let to = trash.join(format!("{}-{}", entry.id, now_secs()));
                std::fs::rename(&dir, &to).map_err(|e| {
                    format!(
                        "cannot move {} to {}: {e}; nothing removed",
                        dir.display(),
                        to.display()
                    )
                })?;
                Some(to)
            } else {
                None
            };
            Removed::Trashed {
                name: entry.name.clone(),
                to,
                was_default,
            }
        } else {
            Removed::Forgotten {
                name: entry.name.clone(),
                dir: dir.clone(),
                was_default,
            }
        };
        registry.profiles.retain(|e| e.id != entry.id);
        if was_default {
            registry.default = None;
        }
        Ok((removed, held))
    })?;
    // The profile's lock, which moved with its directory, is let go only
    // once the registry no longer names it.
    drop(held);
    Ok(removed)
}

/// Whether `name` may be a profile's name, or the sentence that says why
/// not. Taken as typed: nothing is trimmed here.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a profile name is needed".to_string());
    }
    let chars = name.chars().count();
    if chars > MAX_NAME_CHARS {
        return Err(format!(
            "a profile name is at most {MAX_NAME_CHARS} characters; this one is {chars}"
        ));
    }
    if name.contains(['/', '\\']) {
        return Err(format!(
            "a profile name cannot contain / or \\ (\"{}\"); a directory is --profile <dir>, \
             or profiles create <name> --dir <dir>",
            text::sanitize(name)
        ));
    }
    if name.starts_with('-') {
        return Err("a profile name cannot start with -".to_string());
    }
    if !name.chars().all(text::is_plain) {
        return Err(format!(
            "a profile name is plain text; \"{}\" has characters a terminal would not show",
            text::sanitize(name)
        ));
    }
    if name.trim() != name {
        return Err(format!(
            "a profile name cannot start or end with a space (\"{name}\")"
        ));
    }
    Ok(())
}

/// Whether `id` is one an entry may have: `[a-z0-9][a-z0-9-]{0,31}`, so
/// that `profiles/<id>` is always one directory inside `profiles/`.
pub fn valid_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    id.len() <= 32
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A new id: twelve lowercase hex characters from `/dev/urandom`, drawn
/// again while it is one of `taken`'s. Where `/dev/urandom` cannot be read,
/// the clock and the pid, which are unique enough for a list of a handful.
pub fn new_id(taken: &[Entry]) -> String {
    let mut salt = 0u64;
    loop {
        let id = random_hex().unwrap_or_else(|| {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or_default();
            let mixed = nanos ^ (u64::from(std::process::id()) << 20) ^ salt;
            format!("{:012x}", mixed & 0xffff_ffff_ffff)
        });
        if !taken.iter().any(|e| e.id == id) {
            return id;
        }
        salt = salt.wrapping_add(0x9e37_79b9);
    }
}

/// Six random bytes as twelve hex characters, or `None`.
fn random_hex() -> Option<String> {
    let mut bytes = [0u8; 6];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `Some(holder)` while a blinkterm holds the profile at `dir` — the pid it
/// wrote, when that can be read — and `None` while nobody does, or when
/// there is no lock file to ask. The lock, if this gets it, is let go at once.
pub fn in_use(dir: &Path) -> Option<Option<u32>> {
    if !dir.join(profile::LOCK_FILE).is_file() {
        return None;
    }
    match profile::try_lock(dir) {
        Ok(Tried::Held(pid)) => Some(pid),
        Ok(Tried::Taken(_)) | Err(_) => None,
    }
}

/// Which profile `choice` means, from this process's environment: see
/// [`select_in`].
pub fn select(choice: &Choice, ask: Ask) -> Result<Selected, String> {
    select_in(Profile::data_dir(), choice, ask)
}

/// Which profile `choice` means, with `data` the data directory (or the
/// sentence about why there is none).
///
/// `--temp-profile` and `--profile <dir>` never read the registry — a
/// relative directory is made absolute against the working directory, once,
/// here. A name is looked up, and an unknown one is an error rather than a
/// new profile made by a typo. Nothing chosen is the registry's default;
/// with no default, and `--choose-profile` always, it is the picker
/// ([`crate::chooser::pick`]) when `ask` allows it and stdin and stdout are
/// a terminal, and otherwise the sentence that says how to choose. The
/// registry is made, with `Default` in it, by the first of these that reads
/// it.
pub fn select_in(
    data: Result<PathBuf, String>,
    choice: &Choice,
    ask: Ask,
) -> Result<Selected, String> {
    let interactive = || std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    match choice {
        Choice::Temporary => Ok(Selected {
            dir: None,
            id: None,
            label: None,
        }),
        Choice::At(dir) => {
            let dir = if dir.is_absolute() {
                dir.clone()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(dir))
                    .map_err(|e| format!("cannot tell where {} is: {e}", dir.display()))?
            };
            Ok(Selected {
                dir: Some(dir),
                id: None,
                label: None,
            })
        }
        Choice::Named(name) => {
            let registry = Registry::open(&data?)?;
            let entry = registry.by_name(name).ok_or_else(|| unknown_name(name))?;
            Ok(registry.selected(entry))
        }
        Choice::Default => {
            let registry = Registry::open(&data?)?;
            if let Some(entry) = registry.default_entry() {
                return Ok(registry.selected(entry));
            }
            match ask {
                Ask::IfTerminal if interactive() => crate::chooser::pick(registry),
                Ask::IfTerminal => Err(no_default("stdin or stdout is not a terminal to ask in")),
                Ask::Never(why) => Err(no_default(why)),
            }
        }
        Choice::Pick => {
            let registry = Registry::open(&data?)?;
            match ask {
                Ask::IfTerminal if interactive() => crate::chooser::pick(registry),
                Ask::IfTerminal => Err("--choose-profile needs a terminal to ask in".to_string()),
                Ask::Never(why) => Err(format!("--choose-profile and {why}")),
            }
        }
    }
}

/// The sentence for a name the registry does not have.
pub fn unknown_name(name: &str) -> String {
    let name = text::sanitize(name);
    format!(
        "no profile named \"{name}\"; blinkterm profiles list shows them, \
         and blinkterm profiles create {name} makes one"
    )
}

/// The sentence for a name another profile already has, as it is spelled
/// there.
fn taken_name(existing: &str) -> String {
    format!(
        "there is already a profile named \"{}\"",
        text::sanitize(existing)
    )
}

/// The sentence for a start with nothing chosen, no default, and no asking.
fn no_default(why: &str) -> String {
    format!(
        "no default profile, and {why}; say --profile-name <name>, \
         or set one with blinkterm profiles default <name>"
    )
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A scratch data directory of this test's own, gone again before.
    fn scratch(what: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "blinkterm-unit-registry-{what}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path)
            .expect("the path exists")
            .permissions()
            .mode()
            & 0o777
    }

    fn entry(id: &str, name: &str, dir: &str) -> Entry {
        Entry {
            id: id.to_string(),
            name: name.to_string(),
            dir: PathBuf::from(dir),
            created: 1_759_276_800,
        }
    }

    fn two(data: &Path) -> Registry {
        Registry {
            data: data.to_path_buf(),
            default: Some("default".to_string()),
            profiles: vec![
                entry("default", "Default", "profile"),
                entry("3f9a1c0b7e2d", "Work \"q\" 日本", "/home/me/work-profile"),
            ],
        }
    }

    #[test]
    fn what_is_rendered_parses_back_to_itself_and_reads_as_a_list() {
        let data = Path::new("/d");
        let registry = two(data);
        let text = registry.render();
        assert_eq!(Registry::parse(data, &text), Ok(registry.clone()));
        assert_eq!(text.lines().count(), 8, "{text}");
        assert!(text.contains("\n    { \"id\": \"default\", "), "{text}");
        assert!(text.contains("\"version\": 1"), "{text}");

        let empty = Registry {
            data: data.to_path_buf(),
            default: None,
            profiles: Vec::new(),
        };
        let text = empty.render();
        assert!(text.contains("\"default\": null"), "{text}");
        assert_eq!(Registry::parse(data, &text), Ok(empty));
    }

    #[test]
    fn the_initial_registry_is_the_old_profile_called_default() {
        let registry = Registry::initial(Path::new("/d"));
        assert_eq!(registry.profiles.len(), 1);
        let first = &registry.profiles[0];
        assert_eq!(
            (first.id.as_str(), first.name.as_str()),
            ("default", "Default")
        );
        assert_eq!(first.dir, PathBuf::from("profile"));
        assert!(first.is_managed());
        assert_eq!(registry.default_entry(), Some(first));
        assert_eq!(registry.dir_of(first), PathBuf::from("/d/profile"));
    }

    #[test]
    fn open_writes_a_missing_registry_private_and_leaves_an_existing_one_alone() {
        let root = scratch("open");
        let data = root.join("blinkterm");
        let registry = Registry::open(&data).expect("migrated");
        assert_eq!(registry.profiles[0].dir, PathBuf::from(LEGACY_DIR));
        let path = Registry::path(&data);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&data), 0o700);
        assert!(!data.join(LEGACY_DIR).exists(), "nothing is made or moved");

        let hand = "{\"version\": 1, \"default\": null, \"profiles\": []}";
        std::fs::write(&path, hand).expect("a hand edit");
        let again = Registry::open(&data).expect("read");
        assert!(again.profiles.is_empty() && again.default.is_none());
        assert_eq!(std::fs::read_to_string(&path).expect("there"), hand);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_file_that_cannot_be_read_is_refused_by_name_and_never_written_over() {
        let data = Path::new("/d");
        let p = |id: &str, name: &str, dir: &str| {
            format!("{{\"id\": {id:?}, \"name\": {name:?}, \"dir\": {dir:?}}}")
        };
        let doc = |profiles: &[String], default: &str| {
            format!(
                "{{\"version\": 1, \"default\": {default}, \"profiles\": [{}]}}",
                profiles.join(",")
            )
        };
        let big = format!("{{\"x\": \"{}\"}}", "a".repeat(257 * 1024));
        let cases = [
            "not json".to_string(),
            "[]".to_string(),
            "{\"profiles\": []}".to_string(),
            "{\"version\": 0, \"profiles\": []}".to_string(),
            "{\"version\": 1, \"profiles\": {}}".to_string(),
            doc(&[p("a", "Work", "x"), p("b", "work", "y")], "null"),
            doc(&[p("a", "Work", "x"), p("a", "Home", "y")], "null"),
            doc(&[p("../x", "Work", "x")], "null"),
            doc(&[p("Up", "Work", "x")], "null"),
            doc(&[p("", "Work", "x")], "null"),
            doc(&[p("a", "Work", "x")], "\"b\""),
            doc(&[p("a", "Work", "")], "null"),
            doc(&[p("a", "Work", "../elsewhere")], "null"),
            doc(&[p("a", "Work", ".")], "null"),
            doc(&[p("a", "a/b", "x")], "null"),
            big,
        ];
        for text in cases {
            let why = Registry::parse(data, &text).unwrap_err();
            assert!(
                why.starts_with("cannot read /d/profiles.json: "),
                "{}: {why}",
                &text[..text.len().min(80)]
            );
            assert!(why.contains("--profile <dir>"), "{why}");
        }
        let newer = Registry::parse(data, "{\"version\": 2, \"profiles\": []}").unwrap_err();
        assert!(newer.contains("newer blinkterm (version 2"), "{newer}");
        assert!(newer.contains("--profile <dir>"), "{newer}");

        // Keys it does not know are passed over.
        let fine = Registry::parse(
            data,
            "{\"version\": 1, \"x\": 1, \"profiles\": [{\"id\": \"a\", \"name\": \"A\", \"dir\": \"/a\", \"y\": []}]}",
        )
        .expect("read");
        assert_eq!(fine.profiles[0].created, 0);
        assert_eq!(fine.default, None);
    }

    #[test]
    fn a_name_is_plain_text_of_a_sensible_length_with_no_slashes() {
        let refused = [
            ("", "is needed"),
            (&"x".repeat(65), "at most 64 characters; this one is 65"),
            ("a/b", "cannot contain / or \\"),
            ("a\\b", "cannot contain / or \\"),
            ("-x", "cannot start with -"),
            (" x", "space"),
            ("x ", "space"),
            ("a\u{1b}b", "plain text"),
            ("a\u{200b}b", "plain text"),
            ("a\tb", "plain text"),
        ];
        for (name, why) in refused {
            let said = validate_name(name).unwrap_err();
            assert!(said.contains(why), "{name:?}: {said}");
        }
        for name in [
            &"x".repeat(64)[..],
            "Work",
            "仕事",
            "My profile 2",
            "Ünïcødé",
        ] {
            assert_eq!(validate_name(name), Ok(()), "{name:?}");
        }
    }

    #[test]
    fn an_id_is_twelve_hex_characters_and_never_one_already_taken() {
        let id = new_id(&[]);
        assert_eq!(id.len(), 12, "{id}");
        assert!(id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert!(valid_id(&id));
        let taken = vec![entry(&id, "A", "x")];
        assert_ne!(new_id(&taken), id);

        for good in ["default", "a", "0-9", &"a".repeat(32)] {
            assert!(valid_id(good), "{good}");
        }
        for bad in ["", "-a", "A", "a/b", "..", "a.b", &"a".repeat(33)] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn a_managed_profile_is_made_private_under_its_id_and_an_external_one_is_not_touched() {
        let root = scratch("create");
        let data = root.join("blinkterm");
        let work = create(&data, "Work", None).expect("made");
        assert!(work.is_managed());
        assert_eq!(work.dir, Path::new(MANAGED_DIR).join(&work.id));
        assert_eq!(mode(&data.join(&work.dir)), 0o700);
        let registry = Registry::load(&data).expect("read").expect("there");
        assert_eq!(registry.profiles.len(), 2, "Default, then Work");
        assert_eq!(registry.default.as_deref(), Some(DEFAULT_ID));
        assert_eq!(mode(&Registry::path(&data)), 0o600);

        let why = create(&data, "work", None).unwrap_err();
        assert_eq!(why, "there is already a profile named \"Work\"");

        let why = create(&data, "Rel", Some(PathBuf::from("rel"))).unwrap_err();
        assert!(why.contains("absolute --dir"), "{why}");

        let outside = root.join("outside");
        let testing = create(&data, "Testing", Some(outside.clone())).expect("registered");
        assert!(!testing.is_managed());
        assert_eq!(testing.dir, outside);
        assert!(!outside.exists(), "a registered directory is not made");
        let why = create(&data, "Again", Some(outside.clone())).unwrap_err();
        assert_eq!(
            why,
            format!("{} is already the profile \"Testing\"", outside.display())
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_first_profile_of_a_registry_with_no_default_becomes_it() {
        let root = scratch("first-default");
        let data = root.join("blinkterm");
        Registry::open(&data).expect("migrated");
        remove(&data, "Default").expect("removed");
        let registry = Registry::load(&data).expect("read").expect("there");
        assert!(registry.profiles.is_empty() && registry.default.is_none());
        let work = create(&data, "Work", None).expect("made");
        let registry = Registry::load(&data).expect("read").expect("there");
        assert_eq!(registry.default.as_deref(), Some(work.id.as_str()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_rename_changes_the_name_alone() {
        let root = scratch("rename");
        let data = root.join("blinkterm");
        let work = create(&data, "Work", None).expect("made");
        let office = rename(&data, "work", "Office").expect("renamed");
        assert_eq!(
            (office.id.as_str(), &office.dir),
            (work.id.as_str(), &work.dir)
        );
        assert_eq!(office.name, "Office");
        let office = rename(&data, "Office", "OFFICE").expect("a change of case is a rename");
        assert_eq!(office.name, "OFFICE");
        let why = rename(&data, "OFFICE", "default").unwrap_err();
        assert_eq!(why, "there is already a profile named \"Default\"");
        let why = rename(&data, "Nope", "Other").unwrap_err();
        assert!(why.starts_with("no profile named \"Nope\""), "{why}");
        let default = set_default(&data, "office").expect("the default");
        let registry = Registry::load(&data).expect("read").expect("there");
        assert_eq!(registry.default, Some(default.id));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_removed_managed_profile_goes_to_the_trash_and_an_external_one_stays() {
        let root = scratch("remove");
        let data = root.join("blinkterm");
        let work = create(&data, "Work", None).expect("made");
        set_default(&data, "Work").expect("the default");
        let made = data.join(&work.dir);
        std::fs::write(made.join("Cookies"), b"a login").expect("something in it");

        let Removed::Trashed {
            name,
            to: Some(to),
            was_default,
        } = remove(&data, "Work").expect("removed")
        else {
            panic!("a managed profile is trashed");
        };
        assert_eq!(name, "Work");
        assert!(was_default);
        assert!(!made.exists());
        assert!(to.starts_with(data.join(TRASH_DIR)), "{}", to.display());
        assert_eq!(std::fs::read(to.join("Cookies")).expect("kept"), b"a login");
        assert_eq!(mode(&to), 0o700);
        let registry = Registry::load(&data).expect("read").expect("there");
        assert!(registry.by_name("Work").is_none());
        assert_eq!(registry.default, None, "no other profile is promoted");

        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).expect("a directory");
        create(&data, "Testing", Some(outside.clone())).expect("registered");
        let removed = remove(&data, "testing").expect("forgotten");
        assert!(
            matches!(removed, Removed::Forgotten { ref dir, .. } if dir == &std::fs::canonicalize(&outside).unwrap())
        );
        assert!(outside.is_dir(), "a registered directory is left");

        // Default, never started: nothing to move.
        assert!(matches!(
            remove(&data, "Default").expect("removed"),
            Removed::Trashed { to: None, .. }
        ));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_profile_in_use_is_not_removed_and_the_registry_is_untouched() {
        let root = scratch("in-use");
        let data = root.join("blinkterm");
        let work = create(&data, "Work", None).expect("made");
        let dir = data.join(&work.dir);
        let before = std::fs::read(Registry::path(&data)).expect("there");
        assert_eq!(in_use(&dir), None);
        let held = Profile::take(Choice::At(dir.clone())).expect("taken");
        let pid = std::process::id();
        assert_eq!(in_use(&dir), Some(Some(pid)));
        let why = remove(&data, "Work").unwrap_err();
        assert_eq!(
            why,
            format!(
                "the profile \"Work\" is in use by another blinkterm (pid {pid}); quit it first"
            )
        );
        assert!(dir.is_dir());
        assert_eq!(std::fs::read(Registry::path(&data)).expect("there"), before);
        drop(held);
        assert_eq!(in_use(&dir), None);
        remove(&data, "Work").expect("free now");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn two_changes_at_once_both_land() {
        let root = scratch("race");
        let data = root.join("blinkterm");
        Registry::open(&data).expect("migrated");
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let data = data.clone();
                std::thread::spawn(move || create(&data, &format!("P{n}"), None).map(|_| ()))
            })
            .collect();
        for thread in threads {
            thread.join().expect("joined").expect("created");
        }
        let registry = Registry::load(&data).expect("read").expect("there");
        assert_eq!(registry.profiles.len(), 9);
        for n in 0..8 {
            assert!(registry.by_name(&format!("p{n}")).is_some(), "P{n}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn select_reads_the_registry_only_for_a_name_or_nothing() {
        let root = scratch("select");
        let data = root.join("blinkterm");
        let never = Ask::Never("the test cannot ask");

        let temporary = select_in(Ok(data.clone()), &Choice::Temporary, never).expect("temp");
        assert_eq!(temporary.dir, None);
        let at = select_in(Ok(data.clone()), &Choice::At("/p".into()), never).expect("at");
        assert_eq!(at.dir, Some(PathBuf::from("/p")));
        assert_eq!((at.id, at.label), (None, None));
        assert!(!Registry::path(&data).exists(), "neither made a registry");

        let default = select_in(Ok(data.clone()), &Choice::Default, never).expect("default");
        assert_eq!(default.dir, Some(data.join(LEGACY_DIR)));
        assert_eq!(default.label, None, "one profile: no name on the row");
        assert!(Registry::path(&data).exists(), "the first default made it");

        let work = create(&data, "Work", None).expect("made");
        let named =
            select_in(Ok(data.clone()), &Choice::Named("work".into()), never).expect("named");
        assert_eq!(named.dir, Some(data.join(&work.dir)));
        assert_eq!(named.id, Some(work.id));
        assert_eq!(named.label.as_deref(), Some("Work"));
        let default = select_in(Ok(data.clone()), &Choice::Default, never).expect("default");
        assert_eq!(
            default.label.as_deref(),
            Some("Default"),
            "two profiles: named"
        );

        let why = select_in(Ok(data.clone()), &Choice::Named("Nope".into()), never).unwrap_err();
        assert!(why.starts_with("no profile named \"Nope\";"), "{why}");
        assert!(why.contains("blinkterm profiles create Nope"), "{why}");

        remove(&data, "Default").expect("removed");
        let why = select_in(Ok(data.clone()), &Choice::Default, never).unwrap_err();
        assert!(
            why.starts_with("no default profile, and the test cannot ask;"),
            "{why}"
        );
        assert!(why.contains("--profile-name <name>"), "{why}");
        let why = select_in(Ok(data.clone()), &Choice::Pick, never).unwrap_err();
        assert_eq!(why, "--choose-profile and the test cannot ask");

        let why = select_in(Err("no data".into()), &Choice::Default, never).unwrap_err();
        assert_eq!(why, "no data");
        assert!(select_in(Err("no data".into()), &Choice::Temporary, never).is_ok());
        std::fs::remove_dir_all(&root).ok();
    }
}
