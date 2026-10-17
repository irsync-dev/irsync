use irsync::authz::{Access, Authorizer, FileAuthorizer, Grants, MemoryAuthorizer};
use rand_core::OsRng;
use ssh_key::{Algorithm, PrivateKey};

fn pub_line(k: &PrivateKey) -> String {
    k.public_key().to_openssh().unwrap()
}

fn id_of(k: &PrivateKey) -> iroh::EndpointId {
    irsync::identity::endpoint_id_of_public_key(k.public_key()).unwrap()
}

#[test]
fn file_authorizer_parses_modules_option() {
    let k1 = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
    let k2 = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
    let rsa = PrivateKey::from(ssh_key::private::RsaKeypair::random(&mut OsRng, 2048).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("authorized_keys");
    std::fs::write(
        &path,
        format!(
            "# comment\nmodules=\"photos:rw,docs:ro\" {}\nmodules=\"*:ro\" {}\n{}\n",
            pub_line(&k1),
            pub_line(&k2),
            pub_line(&rsa)
        ),
    )
    .unwrap();
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
fn unknown_key_has_no_grants() {
    let k = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
    let other = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
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
