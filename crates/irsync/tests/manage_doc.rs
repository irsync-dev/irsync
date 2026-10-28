use irsync::config::hosts::Hosts;
use irsync::manage::{load_doc, save_doc, ManageError, PeerSpec};
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}

#[test]
fn load_missing_is_empty_and_save_creates_parent_atomically() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("sub/exports.toml");
    let mut doc = load_doc(&p).unwrap();
    assert!(doc.as_table().is_empty());
    doc["x"] = toml_edit::table();
    doc["x"]["path"] = toml_edit::value("/x");
    save_doc(&p, &doc).unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "[x]\npath = \"/x\"\n");
    assert!(
        std::fs::read_dir(p.parent().unwrap()).unwrap().count() == 1,
        "no temp file left behind"
    );
}

#[test]
fn save_preserves_comments_and_order() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("e.toml");
    std::fs::write(
        &p,
        "# top\n[b]\npath = \"/b\" # keep\n\n[a]\npath = \"/a\"\n",
    )
    .unwrap();
    let mut doc = load_doc(&p).unwrap();
    doc["a"]["read_only"] = toml_edit::value(true);
    save_doc(&p, &doc).unwrap();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "# top\n[b]\npath = \"/b\" # keep\n\n[a]\npath = \"/a\"\nread_only = true\n"
    );
}

#[test]
fn peer_spec_forms() {
    let hosts = Hosts::parse(
        &format!("[hosts.laptop]\nendpoint_id = \"{}\"\n", id(1)),
        Path::new("h"),
    )
    .unwrap();
    assert!(
        matches!(PeerSpec::parse("laptop", &hosts).unwrap(), PeerSpec::Alias(ref a) if a == "laptop")
    );
    assert!(
        matches!(PeerSpec::parse(&id(2).to_string(), &hosts).unwrap(), PeerSpec::Id { id: got, .. } if got == id(2))
    );
    let key =
        ssh_key::PrivateKey::random(&mut rand_core::OsRng, ssh_key::Algorithm::Ed25519).unwrap();
    let mut pk = key.public_key().clone();
    pk.set_comment("mike@laptop");
    let line = pk.to_openssh().unwrap();
    let want = irsync::identity::endpoint_id_of_public_key(&pk).unwrap();
    assert!(
        matches!(PeerSpec::parse(&line, &hosts).unwrap(), PeerSpec::Id { id, comment: Some(ref c) } if id == want && c == "mike@laptop")
    );
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("k.pub"), &line).unwrap();
    assert!(matches!(
        PeerSpec::parse(d.path().join("k.pub").to_str().unwrap(), &hosts).unwrap(),
        PeerSpec::Id {
            comment: Some(_),
            ..
        }
    ));
    assert!(matches!(
        PeerSpec::parse("all", &hosts),
        Err(ManageError::NoOpenAccess)
    ));
    assert!(matches!(
        PeerSpec::parse("*", &hosts),
        Err(ManageError::NoOpenAccess)
    ));
    assert!(PeerSpec::parse("nobody", &hosts).is_err());
    assert_eq!(PeerSpec::parse("laptop", &hosts).unwrap().entry(), "laptop");
    assert_eq!(
        PeerSpec::parse(&id(2).to_string(), &hosts).unwrap().entry(),
        id(2).to_string()
    );
}
