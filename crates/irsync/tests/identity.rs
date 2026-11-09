use irsync::identity::{
    endpoint_id_of_public_key, generate_openssh, load_client_identity, load_identity_interactive,
    load_or_generate, load_secret_key, parse_endpoint_id, write_new, IdentityError, KeySource,
    PassphraseSource,
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
fn id_daemon_unlocks_encrypted_identity_from_env_without_tty() {
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
    // `show` also loads the client key, which the config leaves to the
    // default lookup: a home of the test's own, so no real
    // `~/.ssh/id_ed25519` is tried with this passphrase and no key is
    // generated under the real home.
    let home = dir.path().join("home");
    let run = |pass: Option<&str>| {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_irsync"));
        c.args(["id", "show", "--daemon", "--config"])
            .arg(&cfg)
            .env("HOME", &home)
            .env_remove("IRSYNC_PASSPHRASE")
            .stdin(std::process::Stdio::null());
        if let Some(p) = pass {
            c.env("IRSYNC_PASSPHRASE", p);
        }
        c.output().unwrap()
    };
    let o = run(Some("s3cret"));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    let shown = parse_endpoint_id(out.lines().last().unwrap()).unwrap();
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
    // --passphrase-file, as for a sync and the daemon, on `id show` and `show`.
    let pf = dir.path().join("pass");
    std::fs::write(&pf, "s3cret\n").unwrap();
    for args in [&["id", "show", "--daemon"][..], &["show"][..]] {
        let o = std::process::Command::new(env!("CARGO_BIN_EXE_irsync"))
            .args(args)
            .arg("--config")
            .arg(&cfg)
            .arg("--passphrase-file")
            .arg(&pf)
            .env("HOME", &home)
            .env_remove("IRSYNC_PASSPHRASE")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(
            String::from_utf8_lossy(&o.stdout).contains(&shown.to_string()),
            "{args:?}"
        );
    }
}

/// With no client key of irsync's own, the ssh key is the one read, and an
/// error loading it names it, not the default path that does not exist.
#[test]
fn an_ssh_key_that_does_not_unlock_is_named_in_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let ssh = home.join(".ssh/id_ed25519");
    std::fs::create_dir_all(ssh.parent().unwrap()).unwrap();
    PrivateKey::random(&mut OsRng, Algorithm::Ed25519)
        .unwrap()
        .encrypt(&mut OsRng, "the ssh one")
        .unwrap()
        .write_openssh_file(&ssh, LineEnding::LF)
        .unwrap();
    let cfg = dir.path().join("c.toml");
    std::fs::write(
        &cfg,
        format!("state_dir = {:?}\n", dir.path().join("state")),
    )
    .unwrap();
    let pf = dir.path().join("pass");
    std::fs::write(&pf, "another\n").unwrap();
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(["show", "--config"])
        .arg(&cfg)
        .arg("--passphrase-file")
        .arg(&pf)
        .env("HOME", &home)
        .env_remove("IRSYNC_PASSPHRASE")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains(&format!("loading identity {}:", ssh.display())),
        "{err}"
    );
    assert!(!home.join(".config/irsync/id_ed25519").exists());
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

#[test]
fn write_new_never_replaces_an_existing_file() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("k");
    write_new(&p, b"first", 0o600).unwrap();
    let e = write_new(&p, b"second", 0o600).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
    assert_eq!(std::fs::read(&p).unwrap(), b"first");
    assert_eq!(
        std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // No temporary file is left behind.
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
}

#[test]
fn load_or_generate_in_a_read_only_directory_names_the_path() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("ro");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(dir.join("probe"), b"").is_ok() {
        eprintln!("skipping: running as root, mode 0o555 does not stop writes");
        return;
    }
    let p = dir.join("id_ed25519");
    let e = load_or_generate(&p, &PassphraseSource::default(), "irsync").unwrap_err();
    assert!(e.to_string().contains(p.to_str().unwrap()), "{e}");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn client_identity_prefers_the_irsync_key_then_the_ssh_key_then_generates() {
    let d = tempfile::tempdir().unwrap();
    let own = d.path().join("irsync/id_ed25519");
    let ssh = d.path().join("ssh/id_ed25519");
    let src = PassphraseSource::default();
    // Nothing anywhere: generated at the irsync path, the ssh path untouched.
    let (generated, source) = load_client_identity(&own, Some(&ssh), &src, "irsync").unwrap();
    assert_eq!(source, KeySource::Generated(own.clone()));
    assert!(!ssh.exists());
    // The irsync key exists: it wins over an ssh key that appears later.
    std::fs::create_dir_all(ssh.parent().unwrap()).unwrap();
    let ssh_key = generate_openssh(&ssh, "me@laptop").unwrap();
    let (k, source) = load_client_identity(&own, Some(&ssh), &src, "irsync").unwrap();
    assert_eq!(source, KeySource::Loaded(own.clone()));
    assert_eq!(k.public(), generated.public());
    // No irsync key, an ssh key: the ssh key is used as it is, nothing is written.
    std::fs::remove_file(&own).unwrap();
    std::fs::remove_file(d.path().join("irsync/id_ed25519.pub")).unwrap();
    let (k, source) = load_client_identity(&own, Some(&ssh), &src, "irsync").unwrap();
    assert_eq!(source, KeySource::Ssh(ssh.clone()));
    assert_eq!(k.public(), ssh_key.public());
    assert!(!own.exists());
    // A named identity never falls back: it is generated even beside an ssh key.
    let (k, source) = load_client_identity(&own, None, &src, "irsync").unwrap();
    assert_eq!(source, KeySource::Generated(own.clone()));
    assert_ne!(k.public(), ssh_key.public());
}

#[test]
fn an_encrypted_ssh_key_without_a_passphrase_is_an_error_naming_it() {
    let d = tempfile::tempdir().unwrap();
    let own = d.path().join("irsync/id_ed25519");
    let ssh = d.path().join("id_ed25519");
    let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
    key.encrypt(&mut OsRng, "pw")
        .unwrap()
        .write_openssh_file(&ssh, LineEnding::LF)
        .unwrap();
    let e =
        load_client_identity(&own, Some(&ssh), &PassphraseSource::default(), "irsync").unwrap_err();
    assert!(
        matches!(e, IdentityError::NoPassphraseSource(ref q) if q == &ssh),
        "{e}"
    );
    // The passphrase unlocks it; no irsync key is made either way.
    let src = PassphraseSource {
        env: Some("pw".into()),
        ..PassphraseSource::default()
    };
    let (k, source) = load_client_identity(&own, Some(&ssh), &src, "irsync").unwrap();
    assert_eq!(source, KeySource::Ssh(ssh.clone()));
    assert_eq!(
        k.public().as_bytes(),
        &key.public_key().key_data().ed25519().unwrap().0
    );
    assert!(!own.exists());
}
