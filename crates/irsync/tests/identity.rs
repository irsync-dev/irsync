use irsync::identity::{
    endpoint_id_of_public_key, generate_openssh, load_secret_key, parse_endpoint_id, IdentityError,
};
use rand_core::OsRng;
use ssh_key::{Algorithm, LineEnding, PrivateKey};
use std::os::unix::fs::PermissionsExt;

#[test]
fn generated_key_loads_and_public_matches_ssh_pub() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("id_ed25519");
    let generated = generate_openssh(&path, "test@irsync").unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    let loaded = load_secret_key(&path, None).unwrap();
    assert_eq!(loaded.public(), generated.public());

    let pub_text = std::fs::read_to_string(dir.path().join("id_ed25519.pub")).unwrap();
    let ssh_pub = ssh_key::PublicKey::from_openssh(&pub_text).unwrap();
    let ed = ssh_pub.key_data().ed25519().unwrap();
    assert_eq!(loaded.public().as_bytes(), &ed.0);
    assert_eq!(
        endpoint_id_of_public_key(&ssh_pub).unwrap(),
        loaded.public()
    );
}

#[test]
fn encrypted_key_needs_passphrase() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enc");
    let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
    let enc = key.encrypt(&mut OsRng, "hunter2").unwrap();
    enc.write_openssh_file(&path, LineEnding::LF).unwrap();

    assert!(matches!(
        load_secret_key(&path, None),
        Err(IdentityError::PassphraseRequired)
    ));
    let sk = load_secret_key(&path, Some("hunter2")).unwrap();
    assert_eq!(
        sk.public().as_bytes(),
        &key.key_data().ed25519().unwrap().public.0
    );
}

#[test]
fn rsa_key_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rsa");
    let kp = ssh_key::private::RsaKeypair::random(&mut OsRng, 2048).unwrap();
    let key = PrivateKey::from(kp);
    key.write_openssh_file(&path, LineEnding::LF).unwrap();
    assert!(matches!(
        load_secret_key(&path, None),
        Err(IdentityError::NotEd25519)
    ));
    assert!(endpoint_id_of_public_key(key.public_key()).is_none());
}

#[test]
fn parse_endpoint_id_accepts_z32_and_hex() {
    let id = iroh::SecretKey::from_bytes(&[7u8; 32]).public();
    let hex: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(parse_endpoint_id(&hex).unwrap(), id);
    assert_eq!(parse_endpoint_id(&id.to_string()).unwrap(), id);
    assert_eq!(parse_endpoint_id(&id.to_z32()).unwrap(), id);
    assert!(parse_endpoint_id("nonsense").is_err());
}
