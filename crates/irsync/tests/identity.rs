use irsync::identity::{
    endpoint_id_of_public_key, generate_openssh, load_identity_interactive, load_or_generate,
    load_secret_key, parse_endpoint_id, IdentityError, PassphraseSource,
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
    std::fs::write(&path, include_str!("fixtures/rsa_test_key")).unwrap();
    assert!(matches!(
        load_secret_key(&path, None),
        Err(IdentityError::NotEd25519)
    ));
    let pk = ssh_key::PublicKey::from_openssh(include_str!("fixtures/rsa_test_key.pub")).unwrap();
    assert!(endpoint_id_of_public_key(&pk).is_none());
}

#[test]
fn generate_refuses_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("id");
    std::fs::write(&path, "precious").unwrap();
    assert!(matches!(
        generate_openssh(&path, "c"),
        Err(IdentityError::AlreadyExists(_))
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "precious");
    assert!(!dir.path().join("id.pub").exists());

    let path2 = dir.path().join("id2");
    std::fs::write(dir.path().join("id2.pub"), "x").unwrap();
    assert!(matches!(
        generate_openssh(&path2, "c"),
        Err(IdentityError::AlreadyExists(_))
    ));
    assert!(!path2.exists());
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

#[test]
fn encrypted_identity_via_env_and_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enc");
    let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
    key.encrypt(&mut OsRng, "hunter2")
        .unwrap()
        .write_openssh_file(&path, LineEnding::LF)
        .unwrap();
    let want = key.key_data().ed25519().unwrap().public.0;
    // A non-interactive source (no tty): nothing to ask.
    let none = PassphraseSource::default();
    assert!(matches!(
        load_identity_interactive(&path, &none),
        Err(IdentityError::NoPassphraseSource(_))
    ));
    // From the environment.
    let env = PassphraseSource {
        env: Some("hunter2".into()),
        ..PassphraseSource::default()
    };
    let sk = load_identity_interactive(&path, &env).unwrap();
    assert_eq!(sk.public().as_bytes(), &want);
    // From a file (first line, trailing newline ignored); it wins over env.
    let file = dir.path().join("pass");
    std::fs::write(&file, "hunter2\n").unwrap();
    let from_file = PassphraseSource {
        file: Some(file),
        env: Some("wrong".into()),
        prompt: false,
    };
    let sk = load_identity_interactive(&path, &from_file).unwrap();
    assert_eq!(sk.public().as_bytes(), &want);
    // A wrong passphrase says so.
    let wrong = PassphraseSource {
        env: Some("wrong".into()),
        ..PassphraseSource::default()
    };
    assert!(matches!(
        load_identity_interactive(&path, &wrong),
        Err(IdentityError::BadPassphrase(_))
    ));
    // An unencrypted key never asks.
    let plain = dir.path().join("plain");
    generate_openssh(&plain, "x").unwrap();
    assert!(load_identity_interactive(&plain, &none).is_ok());
}

#[test]
fn daemon_show_id_unlocks_encrypted_identity_from_env_without_tty() {
    let dir = tempfile::tempdir().unwrap();
    let id = dir.path().join("id");
    let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
    key.encrypt(&mut OsRng, "s3cret")
        .unwrap()
        .write_openssh_file(&id, LineEnding::LF)
        .unwrap();
    let cfg = dir.path().join("d.toml");
    std::fs::write(
        &cfg,
        format!(
            "state_dir = {:?}\n[daemon]\nidentity = {:?}\n",
            dir.path().join("state"),
            id
        ),
    )
    .unwrap();
    let run = |pass: Option<&str>| {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_irsync"));
        c.args(["daemon", "--show-id", "--config"])
            .arg(&cfg)
            .env_remove("IRSYNC_PASSPHRASE")
            .stdin(std::process::Stdio::null());
        if let Some(p) = pass {
            c.env("IRSYNC_PASSPHRASE", p);
        }
        c.output().unwrap()
    };
    let o = run(Some("s3cret"));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let shown = parse_endpoint_id(String::from_utf8(o.stdout).unwrap().trim()).unwrap();
    assert_eq!(
        shown.as_bytes(),
        &key.key_data().ed25519().unwrap().public.0
    );
    let o = run(None);
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("IRSYNC_PASSPHRASE"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn load_or_generate_creates_once_then_loads_same_key() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("deep/dir/id_ed25519");
    let src = PassphraseSource::default();
    let (k1, gen1) = load_or_generate(&p, &src, "irsync").unwrap();
    let (k2, gen2) = load_or_generate(&p, &src, "irsync").unwrap();
    assert!(gen1 && !gen2);
    assert_eq!(k1.public(), k2.public());
    assert!(d.path().join("deep/dir/id_ed25519.pub").exists());
    assert_eq!(
        std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn encrypted_existing_key_without_passphrase_is_error_not_regenerated() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("k");
    let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
    let enc = key.encrypt(&mut OsRng, "pw").unwrap();
    enc.write_openssh_file(&p, LineEnding::LF).unwrap();
    let before = std::fs::read(&p).unwrap();
    let e = load_or_generate(&p, &PassphraseSource::default(), "irsync").unwrap_err();
    assert!(
        matches!(e, IdentityError::NoPassphraseSource(ref q) if q == &p),
        "{e}"
    );
    assert_eq!(std::fs::read(&p).unwrap(), before);
}
