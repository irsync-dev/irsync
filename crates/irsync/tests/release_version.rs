#[test]
fn release_package_version_matches_the_workspace() {
    let dist: toml::Table = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packaging/dist/dist.toml"
    ))
    .unwrap()
    .parse()
    .unwrap();
    assert_eq!(
        dist["package"]["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION")),
        "bump packaging/dist/dist.toml together with [workspace.package] version"
    );
}

/// The release notes of this version are written: `CHANGELOG.md` has the
/// heading `## v<version>` once, with text under it. dist puts that section
/// at the top of the GitHub Release, and the release gate makes the same
/// check (`.github/scripts/changelog-section.sh`). Here it fails the pull
/// request that bumps the version without the notes.
#[test]
fn the_changelog_has_notes_for_this_version() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../CHANGELOG.md"))
        .expect("CHANGELOG.md at the root of the repository");
    let heading = concat!("## v", env!("CARGO_PKG_VERSION"));
    let lines: Vec<&str> = text.lines().collect();
    let at: Vec<usize> = (0..lines.len()).filter(|&i| lines[i] == heading).collect();
    assert_eq!(
        at.len(),
        1,
        "CHANGELOG.md must have the heading '{heading}' exactly once"
    );
    let written = lines[at[0] + 1..]
        .iter()
        .take_while(|l| !l.starts_with("## "))
        .any(|l| !l.trim().is_empty());
    assert!(
        written,
        "nothing is written under '{heading}' in CHANGELOG.md"
    );
}
