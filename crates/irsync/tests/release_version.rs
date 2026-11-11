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
