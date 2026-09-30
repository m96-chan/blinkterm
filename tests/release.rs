//! The release, held together before there is a tag.
//!
//! A release is a promise made in four files that nothing compiles together.
//! `Cargo.toml`'s `[package.metadata.binstall]` tells `cargo binstall` where an
//! archive is and what is inside it; `.github/workflows/release.yml` makes the
//! archives and names them; `CHANGELOG.md` has to have the section the
//! workflow cuts into the release notes; and `docs/install.md` lists the
//! targets somebody can download. A rename in any one of them breaks the
//! release on the day it is tagged, and only then — the workflow runs on tags,
//! and `cargo binstall` finds out when somebody runs it.
//!
//! So these tests read the four files as text and hold them to each other, as
//! `bindings.rs` holds `docs/configuration.md` to `ACTIONS`. They run under a
//! plain `cargo test`, with no engine and no network: they cannot say that an
//! archive downloads, only that the names somebody would download it by agree.

use std::collections::BTreeSet;
use std::process::{Command, Stdio};

const CARGO_TOML: &str = include_str!("../Cargo.toml");
const WORKFLOW: &str = include_str!("../.github/workflows/release.yml");
const CHANGELOG: &str = include_str!("../CHANGELOG.md");
const INSTALL: &str = include_str!("../docs/install.md");

/// Every target a release has an archive for. A seventh goes in the matrix,
/// the install doc's table and here, or a test below says which is missing.
const TARGETS: [&str; 6] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
];

/// The value of `key = "..."` in the binstall table, as written.
fn binstall(key: &str) -> &'static str {
    let table = CARGO_TOML
        .split("[package.metadata.binstall]")
        .nth(1)
        .expect("Cargo.toml has a [package.metadata.binstall] table");
    let prefix = format!("{key} = \"");
    table
        .lines()
        .take_while(|line| !line.starts_with('['))
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or_else(|| panic!("the binstall table has no {key}"))
}

/// What the release workflow's notes step keeps of `changelog` for
/// `version`: the lines after the `## [version]` heading, up to the next
/// `## [`. The same as its `awk`, which is
/// `/^## \[/ { p = ($2 == "[" v "]"); next } p`.
fn section(changelog: &str, version: &str) -> String {
    let wanted = format!("[{version}]");
    let mut keep = false;
    let mut out = String::new();
    for line in changelog.lines() {
        if line.starts_with("## [") {
            keep = line.split_whitespace().nth(1) == Some(wanted.as_str());
            continue;
        }
        if keep {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[test]
fn the_binary_says_the_version_cargo_toml_says() {
    let out = Command::new(env!("CARGO_BIN_EXE_blinkterm"))
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .expect("the binary runs");
    assert!(out.status.success(), "--version failed: {out:?}");
    // What `verify` compares the tag against and every `build` job compares
    // the built binary against.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("blinkterm {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn the_binstall_metadata_names_the_archives_the_release_workflow_makes() {
    let url = binstall("pkg-url");
    let bin_dir = binstall("bin-dir");
    assert_eq!(
        url,
        "{ repo }/releases/download/v{ version }/{ name }-{ version }-{ target }.tar.gz"
    );
    assert_eq!(
        bin_dir,
        "{ name }-{ version }-{ target }/{ bin }{ binary-ext }"
    );
    assert_eq!(binstall("pkg-fmt"), "tgz");

    // The workflow packs a directory named like the archive, into the archive.
    assert!(
        WORKFLOW.contains(r#"archive="blinkterm-${VERSION}-${TARGET}""#),
        "release.yml no longer names the archive blinkterm-<version>-<target>"
    );
    assert!(WORKFLOW.contains(r#"tar -C dist -czf "dist/$archive.tar.gz" "$archive""#));
    // The tag binstall looks under is the one the workflow runs on.
    assert!(WORKFLOW.contains(r#"tags: ["v*"]"#));

    // The directory binstall looks for the binary in is the archive's own
    // name: the url's last part with the suffix off.
    let file = url.rsplit('/').next().expect("a url has a last part");
    let stem = file
        .strip_suffix(".tar.gz")
        .expect("the archives are .tar.gz");
    let dir = bin_dir.split('/').next().expect("bin-dir has a directory");
    assert_eq!(stem, dir);
    // And that name, filled in, is the workflow's.
    assert_eq!(
        stem.replace("{ name }", "blinkterm")
            .replace("{ version }", "${VERSION}")
            .replace("{ target }", "${TARGET}"),
        "blinkterm-${VERSION}-${TARGET}"
    );
}

#[test]
fn the_changelog_has_a_section_for_the_version_cargo_toml_says() {
    let version = env!("CARGO_PKG_VERSION");
    let heading = format!("## [{version}] - ");
    assert!(
        CHANGELOG.lines().any(|line| line.starts_with(&heading)),
        "CHANGELOG.md has no `{heading}<date>` heading; the release notes would be empty"
    );
    assert!(
        section(CHANGELOG, version)
            .lines()
            .any(|line| !line.trim().is_empty()),
        "CHANGELOG.md's [{version}] section says nothing"
    );
}

#[test]
fn the_release_workflow_builds_every_target_the_install_doc_lists() {
    let built: BTreeSet<&str> = WORKFLOW
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- { target: "))
        .map(|rest| rest.split(',').next().expect("a target").trim())
        .collect();
    let listed: BTreeSet<String> = INSTALL
        .lines()
        .skip_while(|line| *line != "### Prebuilt binaries")
        .skip(1)
        .take_while(|line| !line.starts_with('#'))
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            let cell = line
                .trim_matches('|')
                .split('|')
                .next()
                .expect("a first cell");
            cell.replace('`', "").trim().to_string()
        })
        .collect();
    let listed: BTreeSet<&str> = listed.iter().map(String::as_str).collect();
    let expected: BTreeSet<&str> = TARGETS.into_iter().collect();

    assert_eq!(
        built, expected,
        "release.yml's matrix against the targets a release has"
    );
    assert_eq!(
        listed, expected,
        "docs/install.md's table against the targets a release has"
    );
}

#[test]
fn the_changelog_section_the_workflow_cuts_is_the_one_between_two_headings() {
    let fixture = "\
# Changelog
## [Unreleased]
- not yet
## [1.2.0] - 2026-01-01
### Fixed
- a thing
## [1.1.0] - 2025-12-01
- older
";
    assert_eq!(section(fixture, "1.2.0"), "### Fixed\n- a thing\n");
    assert_eq!(section(fixture, "1.1.0"), "- older\n");
    assert_eq!(
        section(fixture, "1.1"),
        "",
        "a prefix of a version is not the version"
    );
    assert_eq!(section(fixture, "Unreleased"), "- not yet\n");

    // And on the real one: 0.3.0's section is what its hand-made release
    // notes were, from `### Added` on.
    let real = section(CHANGELOG, "0.3.0");
    assert!(
        real.trim_start().starts_with("### Added"),
        "0.3.0's section: {real:.200}"
    );
    assert!(!real.contains("## [0.2.0]"));

    // The workflow does in awk what `section` does here.
    assert!(WORKFLOW.contains(
        r#"awk -v v="$VERSION" '/^## \[/ { p = ($2 == "[" v "]"); next } p' CHANGELOG.md > notes.md"#
    ));
}
