use irsync::authz::{Access, Authorizer, FileAuthorizer, Grants, MemoryAuthorizer};
use rand_core::OsRng;
use ssh_key::{Algorithm, PrivateKey};
use std::path::PathBuf;

const RSA_PUB: &str = include_str!("fixtures/rsa_test_key.pub");

fn new_key() -> PrivateKey {
    PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap()
}

fn pub_line(k: &PrivateKey) -> String {
    k.public_key().to_openssh().unwrap()
}

fn id_of(k: &PrivateKey) -> iroh::EndpointId {
    irsync::identity::endpoint_id_of_public_key(k.public_key()).unwrap()
}

fn write_keys(contents: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("authorized_keys");
    std::fs::write(&path, contents).unwrap();
    (dir, path)
}

#[test]
fn file_authorizer_parses_modules_option() {
    let k1 = new_key();
    let k2 = new_key();
    let (_d, path) = write_keys(&format!(
        "# comment\nmodules=\"photos:rw,docs:ro\" {}\nmodules=\"*:ro\" {}\n{}\n",
        pub_line(&k1),
        pub_line(&k2),
        RSA_PUB.trim()
    ));
    let a = FileAuthorizer::load(&path).unwrap();
    let g1 = a.grants(&id_of(&k1)).unwrap();
    assert!(g1.allows("photos", true));
    assert!(g1.allows("docs", false));
    assert!(!g1.allows("docs", true));
    assert!(!g1.allows("other", false));
    let g2 = a.grants(&id_of(&k2)).unwrap();
    assert!(g2.allows("anything", false));
    assert!(!g2.allows("anything", true));
}

#[test]
fn flag_and_from_options_before_modules() {
    let k1 = new_key();
    let k2 = new_key();
    let (_d, path) = write_keys(&format!(
        "restrict,modules=\"a:rw\" {}\nfrom=\"10.0.0.0/8\",no-pty,modules=\"b:ro,c:rw\" {}\n",
        pub_line(&k1),
        pub_line(&k2),
    ));
    let a = FileAuthorizer::load(&path).unwrap();
    assert!(a.grants(&id_of(&k1)).unwrap().allows("a", true));
    let g2 = a.grants(&id_of(&k2)).unwrap();
    assert!(g2.allows("b", false));
    assert!(!g2.allows("b", true));
    assert!(g2.allows("c", true));
}

#[test]
fn trailing_comment_after_key_is_accepted() {
    let k = new_key();
    let (_d, path) = write_keys(&format!("modules=\"a:rw\" {} alice laptop\n", pub_line(&k)));
    let a = FileAuthorizer::load(&path).unwrap();
    assert!(a.grants(&id_of(&k)).unwrap().allows("a", true));
}

#[test]
fn malformed_modules_value_is_an_error() {
    let k = new_key();
    for bad in ["a", "a:xx", ":rw", "a:rw,,b:ro", ""] {
        let (_d, path) = write_keys(&format!("modules=\"{bad}\" {}\n", pub_line(&k)));
        assert!(FileAuthorizer::load(&path).is_err(), "accepted {bad:?}");
    }
}

#[test]
fn duplicate_key_first_entry_wins() {
    let k = new_key();
    let (_d, path) = write_keys(&format!(
        "modules=\"a:ro\" {}\nmodules=\"a:rw\" {}\n",
        pub_line(&k),
        pub_line(&k)
    ));
    let a = FileAuthorizer::load(&path).unwrap();
    assert!(!a.grants(&id_of(&k)).unwrap().allows("a", true));
}

#[test]
fn unknown_key_has_no_grants() {
    let k = new_key();
    let other = new_key();
    let m = MemoryAuthorizer::single(
        id_of(&k),
        Grants {
            all: Some(Access::ReadWrite),
            ..Default::default()
        },
    );
    assert!(m.grants(&id_of(&k)).unwrap().allows("x", true));
    assert!(m.grants(&id_of(&other)).is_none());
}
