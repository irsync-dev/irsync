# Host Aliases in Immediate Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A first sync over ssh saves aliases a person would type (`host` for `user@host`, the server's host name for an IP address), and the server knows the client by its host name instead of a bare EndpointId.

**Architecture:** The ssh probe also returns the server's host name. The client picks the aliases to save from the typed destination and that name, and finds a saved host by its `ssh` value before it falls back to ssh setup. `ssh-setup` takes a new `--client-name`, checks it before changing anything, saves it in the server's `[hosts]` and grants the name. Automatic exports write the alias for a peer when `[hosts]` has one.

**Tech Stack:** Rust, clap, toml_edit, nix (`gethostname`), the fake-ssh test fixture.

**Spec:** `docs/superpowers/specs/2026-11-17-immediate-aliases-design.md`

## Global Constraints

- The client's name is a label only. Access is always decided on the EndpointId it resolves to.
- Names reach the config only through `toml_edit` keys and string values, never through formatted TOML text.
- The server is not changed (no install, no identity, no grant) for a name that is then refused.
- No migration: bare-id grants and existing aliases keep working untouched.
- No protocol change: the name travels as an ssh command argument only.
- Short host names: everything from the first `.` on is cut.
- Examples and test names use `myclient` and `myserver`; home paths are written `~/...`.
- Commit messages: a short summary line, a blank line, a brief plain reason. No attribution trailers of any kind.
- Everyday test run: `cargo test -q --workspace --all-features --no-fail-fast`. Before the last commit: `tools/ci.sh check`.

## Review Focus

1. **A client host name with shell characters** (`my client`, `a'b`, `$(x)`): it is passed in an ssh command line. Expected: it arrives on the server byte for byte, or is refused; it never runs as shell. Test in Task 5.
2. **A host name that needs TOML quoting** (`my.client` before cutting is not the case; use `my client` and `"q"`): expected: the server config still loads and the name round-trips. Test in Task 6.
3. **The server's `[hosts]` entry for a client is removed by hand while exports still grant the name:** expected: the config does not silently grant anyone else; whatever the loader does today with an unknown alias in `rw` stays as it is. Test in Task 7.
4. **Server host name `localhost`, empty, or an EndpointId-looking string:** expected: `localhost` and unusable names count as "no usable name" (IPv4 falls back to the address, IPv6 fails before any change). Test in Task 2.
5. **Typing `user@1.2.3.4` after `1.2.3.4` was saved (or the other way round):** the `ssh` values differ, so the second form runs ssh setup and must end on the same alias (same id), not on `myserver-2`. Test in Task 3.

---

## File Structure

| File | Change |
|------|--------|
| `crates/irsync/src/cli/location.rs` | Accept `[ipv6]:PATH` and `user@[ipv6]:PATH`. |
| `crates/irsync/src/config/hosts.rs` | `short_host_name`, `Hosts::by_ssh`. |
| `crates/irsync/src/ssh/client.rs` | Probe `hostname`; `client_host_name`; `run_setup` sends `--client-name`. |
| `crates/irsync/src/ssh/onboard.rs` | Choose and save the aliases; report them. |
| `crates/irsync/src/ssh/setup.rs` | Check the client name, save it in `[hosts]`, grant the name. |
| `crates/irsync/src/cli/mod.rs`, `crates/irsync/src/main.rs` | `--client-name` flag; lookup by ssh destination in `connect_for`. |
| `crates/irsync/src/manage/exports.rs`, `crates/irsync/src/authz.rs` | Automatic exports write the alias. |
| `crates/irsync/tests/fixtures/fake-ssh`, `crates/irsync/tests/onboard_e2e.rs` | Fake `hostname`; new and updated end-to-end tests. |
| `book/src/...` | Five pages, see Task 8. |

---

### Task 1: Bracketed IPv6 in a remote operand

Today the host is the text before the first `:`, so an IPv6 address cannot be typed.

**Files:**
- Modify: `crates/irsync/src/cli/location.rs` (`Location::parse`)
- Test: the existing tests of `Location::parse` (find them with `grep -rn "Location::parse" crates/irsync`)

**Interfaces:**
- Produces: `Location::Remote.host` holds the address **without brackets**: `[fe80::1]:/p` gives host `fe80::1`; `mike@[fe80::1]:/p` gives `mike@fe80::1`. This is the form ssh takes as a destination.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn bracketed_ipv6_host() {
    let host = |s: &str| match Location::parse(s).unwrap() {
        Location::Remote { host, path, .. } => (host, path),
        l => panic!("{l:?}"),
    };
    assert_eq!(host("[fe80::1]:/p"), ("fe80::1".into(), b"/p".to_vec()));
    assert_eq!(host("mike@[::1]:dir/"), ("mike@::1".into(), b"dir".to_vec()));
    // Not an address in the brackets, or no closing bracket:
    assert!(Location::parse("[myserver]:/p").is_err());
    assert!(Location::parse("[fe80::1:/p").is_err());
    // A local path may still start with a bracket.
    assert!(!Location::parse("[fe80::1]/p").unwrap().is_remote());
    assert!(!Location::parse("[notes]/a").unwrap().is_remote());
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync bracketed_ipv6_host`
Expected: FAIL (host is `[fe80`).

- [ ] **Step 3: Implement**

In `Location::parse`, before the existing colon rule: if the string, after an optional `user@` prefix with no `/` or `:` in it, starts with `[`, and the matching `]` comes before any `/` and is followed by `:`, then the text in the brackets must parse as `std::net::Ipv6Addr` (else `UsageError`), the host is `user@` plus the address text, and the rest after `]:` is the path, handled as today. A `[` with no `]:` before the first `/` falls through to the existing rule unchanged, except `[fe80::1:/p` (no `]` at all, colon before slash), which is a `UsageError`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p irsync location`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/cli/location.rs
git commit -m "Accept a bracketed IPv6 address as the remote host"
```

---

### Task 2: Short host names and the server's name from the probe

**Files:**
- Modify: `crates/irsync/src/config/hosts.rs`, `crates/irsync/src/ssh/client.rs` (`Probe`, `probe`), `crates/irsync/tests/fixtures/fake-ssh`
- Test: unit tests in both source files

**Interfaces:**
- Produces:
  - `pub fn short_host_name(raw: &str) -> Option<String>` in `config/hosts.rs`: trims, cuts at the first `.`, and returns the result only when it passes `valid_alias`, is not `localhost`, and does not parse as an IP address.
  - `Probe.hostname: Option<String>`: already passed through `short_host_name`.
  - fake-ssh: env `FAKE_SSH_HOSTNAME` puts a `hostname` stand-in first on `PATH`, like `FAKE_SSH_UNAME`. The special value `-` makes the stand-in print nothing.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn short_host_names() {
    assert_eq!(short_host_name("myserver.example.com\n").as_deref(), Some("myserver"));
    assert_eq!(short_host_name("my server").as_deref(), Some("my server"));
    for bad in ["", "  ", ".local", "localhost", "localhost.localdomain",
                "all", "*", "a/b", "a:b", "192.168.1.5", "10"] {
        assert_eq!(short_host_name(bad), None, "{bad:?}");
    }
}
```

`"192.168.1.5"` cuts to `192`; `"10"` is a bare number. Both are refused by one rule: a name made only of ASCII digits is not usable. Add an EndpointId-looking string to the list using the test module's existing id helper.

In `ssh/client.rs`, next to the existing probe parsing tests, add one that feeds probe output with a host name line and one without, and asserts `hostname`. The probe prints the host name on its own line **after** the version line, so older parsing positions do not move:

```
echo MARKER; id -u; uname -sm; "$HOME/BIN" --version 2>/dev/null || echo; hostname 2>/dev/null || true
```

Note the `|| echo`: the version line must exist (empty when there is no binary) so the host name is always the fourth line.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync short_host_names`
Expected: FAIL to compile (`short_host_name` not found).

- [ ] **Step 3: Implement** `short_host_name`, the `Probe.hostname` field, the probe command and its parsing, and `FAKE_SSH_HOSTNAME` in the fixture.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p irsync --lib ssh::client config::hosts && cargo test -q -p irsync --test onboard_e2e`
Expected: PASS (the end-to-end tests still pass: nothing uses the name yet).

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/config/hosts.rs crates/irsync/src/ssh/client.rs crates/irsync/tests/fixtures/fake-ssh
git commit -m "Ask the server for its host name in the ssh probe"
```

---

### Task 3: Choose and save the aliases on the client

**Files:**
- Modify: `crates/irsync/src/ssh/onboard.rs`, `crates/irsync/src/main.rs` (`connect_for`, `recheck_over_ssh`)
- Test: unit tests in `onboard.rs`; `crates/irsync/tests/onboard_e2e.rs`

**Interfaces:**
- Consumes: `Probe.hostname`, `short_host_name` (Task 2).
- Produces, all in `ssh/onboard.rs`:

```rust
/// What the text before the `:` is, as an ssh destination.
pub enum DestKind { Ip { v6: bool }, UserHost { host: String }, Plain }
pub fn dest_kind(dest: &str) -> DestKind;

/// Before any change on the server. `server_name`: `Probe.hostname`.
/// Errors: dest starts with `-`; an IPv6 dest with no server name;
/// a non-IP dest that is not a valid alias.
pub fn check_first(dest: &str, server_name: Option<&str>) -> anyhow::Result<()>;

/// The aliases to save for a first run, the main one first.
pub fn choose_aliases(
    dest: &str,
    server_name: Option<&str>,
    id: EndpointId,
    existing: &dyn Fn(&str) -> Option<EndpointId>,
) -> Vec<String>;

pub struct SavedAlias {
    pub alias: String,
    /// The id it pointed at before, when it changed.
    pub previous_id: Option<EndpointId>,
    /// It was saved before this run.
    pub existed: bool,
}
```

  - `Onboarded`: `alias` stays (the main alias). `previous_id` and `alias_existed` are replaced by `pub saved: Vec<SavedAlias>` (main alias first).
  - `onboard(paths, rsh, alias: Option<&str>, dest, client, recheck, no_dht)`: `None` is a first run (aliases are chosen); `Some(a)` is a saved host being checked (only `a` is updated, as today).
  - `check(alias, dest)` keeps its meaning for a saved host. The early call in `connect_for` (before the client key is loaded, before any ssh call) stays `check(host, &dest)` for a saved host. For a first run it rejects only a `-` prefix and a non-IP dest that is not a valid alias; an IP dest passes there. `check_first` runs inside `onboard`, right after the probe and the root check and before `ensure_installed`: that is where an IPv6 dest with no server name is refused.

Rules for `choose_aliases` (the `dest` host part is the text after the last `@`):

| Destination | Server name | Result |
|-------------|-------------|--------|
| IP (v4 or v6, with or without `user@`) | usable | `[name]` if `existing(name)` is `None` or `Some(id)`; else the first of `name-2`, `name-3`, ... for which that holds |
| IPv4 | none | `[dest]` |
| `user@host` | any | `[dest, host]` if `existing(host)` is `None` and `host` is a valid alias; else `[dest]` |
| anything else | any | `[dest]` |

Every saved alias gets `ssh = dest` (as typed). In the `user@host` case the plain `host` alias is only ever **created**, never updated: when `existing(host)` is `Some(_)`, whatever its id, it is left alone.

- [ ] **Step 1: Write the failing unit tests** in `onboard.rs`, using the module's `id(b)` helper:

```rust
#[test]
fn chooses_aliases() {
    let none = |_: &str| None;
    let pick = |dest, name, ex: &dyn Fn(&str) -> Option<EndpointId>| {
        choose_aliases(dest, name, id(1), ex)
    };
    assert_eq!(pick("myserver", None, &none), ["myserver"]);
    assert_eq!(pick("mike@myserver", Some("x"), &none), ["mike@myserver", "myserver"]);
    // First one wins: plain name taken, by anyone.
    let taken = |a: &str| (a == "myserver").then(|| id(2));
    assert_eq!(pick("bob@myserver", None, &taken), ["bob@myserver"]);
    let own = |a: &str| (a == "myserver").then(|| id(1));
    assert_eq!(pick("bob@myserver", None, &own), ["bob@myserver"]);
    // IP addresses take the server's name.
    assert_eq!(pick("192.168.1.5", Some("myserver"), &none), ["myserver"]);
    assert_eq!(pick("mike@192.168.1.5", Some("myserver"), &none), ["myserver"]);
    assert_eq!(pick("fe80::1", Some("myserver"), &none), ["myserver"]);
    assert_eq!(pick("192.168.1.5", Some("myserver"), &own), ["myserver"]);
    assert_eq!(pick("192.168.1.5", Some("myserver"), &taken), ["myserver-2"]);
    let two = |a: &str| match a {
        "myserver" => Some(id(2)),
        "myserver-2" => Some(id(1)),
        _ => None,
    };
    assert_eq!(pick("192.168.1.5", Some("myserver"), &two), ["myserver-2"]);
    assert_eq!(pick("192.168.1.5", None, &none), ["192.168.1.5"]);
}

#[test]
fn first_run_checks() {
    assert!(check_first("192.168.1.5", None).is_ok());
    assert!(check_first("fe80::1", Some("myserver")).is_ok());
    let e = check_first("mike@fe80::1", None).unwrap_err().to_string();
    assert!(e.contains("host name"), "{e}");
    assert!(check_first("-oProxyCommand=x", None).is_err());
    assert!(check_first("all", None).is_err());
}
```

- [ ] **Step 2: Write the failing end-to-end tests** in `onboard_e2e.rs`. Add `hostname: Option<String>` to `World` (default `None`: `FAKE_SSH_HOSTNAME` unset is not allowed to leak the real machine name into assertions, so `irsync_bare` sets `FAKE_SSH_HOSTNAME` to the value, or to `-` when `None`).

```rust
#[test]
fn user_at_host_saves_the_plain_name_too() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &format!("u@myserver:{}", dest.display())]));
    assert!(err.contains("saved host alias u@myserver"), "{err}");
    assert!(err.contains("saved host alias myserver"), "{err}");
    let cfg: toml::Table = w.client_config().parse().unwrap();
    for a in ["u@myserver", "myserver"] {
        assert_eq!(cfg["hosts"][a]["ssh"].as_str(), Some("u@myserver"), "{a}");
    }
    let calls = w.ssh_calls();
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    assert_eq!(w.ssh_calls(), calls, "the plain name needs no ssh");
}

#[test]
fn ip_address_is_saved_under_the_server_name() {
    let mut w = World::new();
    w.hostname = Some("myserver.example.com".into());
    let dest = w.server.path().join("dest");
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &format!("192.168.1.5:{}", dest.display())]));
    assert!(err.contains("saved host alias myserver"), "{err}");
    let cfg: toml::Table = w.client_config().parse().unwrap();
    assert_eq!(cfg["hosts"]["myserver"]["ssh"].as_str(), Some("192.168.1.5"));
    assert!(cfg["hosts"].get("192.168.1.5").is_none());
    same_tree(w.src.path(), &dest);
    // Review Focus 5: the other spelling ends on the same alias.
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("u@192.168.1.5:{}", dest.display())]));
    let cfg: toml::Table = w.client_config().parse().unwrap();
    assert_eq!(cfg["hosts"].as_table().unwrap().len(), 1, "{cfg}");
    assert_eq!(cfg["hosts"]["myserver"]["ssh"].as_str(), Some("u@192.168.1.5"));
}

#[test]
fn ipv6_without_a_server_name_changes_nothing() {
    let w = World::new(); // hostname: None
    let out = w.irsync(&["-a", &w.src_arg(), "[fe80::1]:dest"]);
    assert!(!out.status.success());
    assert_eq!(w.ssh_calls(), 1, "the probe only");
    assert!(!w.server.path().join(".local/share/irsync").exists());
    assert_eq!(w.client_config(), "");
}
```

Update `user_at_host_alias` only if its ssh call count changes.

- [ ] **Step 3: Run them and see them fail**

Run: `cargo test -q -p irsync chooses_aliases first_run_checks`
Expected: FAIL to compile.

- [ ] **Step 4: Implement** the items in Interfaces. In `onboard`, the order is: early check, probe, root check, `check_first(dest, p.hostname.as_deref())` on a first run, install, `run_setup`, then under the config lock `choose_aliases` (with `existing` reading the locked document) and one `host_add` per alias. `summary` prints one "saved host alias" or "updated host alias" line per `SavedAlias`, by today's rules, and uses the main alias where it used `o.alias`.

In `connect_for`: pass `None` on a first run and `Some(host)` otherwise; after onboarding, the lookups that follow (`hosts.get(host)`, `resolve_target`) use `o.alias` instead of the typed text. `recheck_over_ssh` passes `Some(host)`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -q -p irsync --lib ssh::onboard && cargo test -q -p irsync --test onboard_e2e`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/irsync/src/ssh/onboard.rs crates/irsync/src/main.rs crates/irsync/tests/onboard_e2e.rs
git commit -m "Save the plain host name and the server's name as aliases

A first sync to user@host also saves host when that name is free, and
a first sync to an IP address saves the server's own host name instead
of the address."
```

---

### Task 4: Find a saved host by its ssh destination

**Files:**
- Modify: `crates/irsync/src/config/hosts.rs`, `crates/irsync/src/main.rs` (`connect_for`)
- Test: unit test in `hosts.rs`; `crates/irsync/tests/onboard_e2e.rs`

**Interfaces:**
- Produces: in `impl Hosts`:

```rust
/// The alias whose `ssh` value is exactly `dest`: the first in
/// lexicographic order when several entries with one id have it.
/// `Err`: the aliases, when entries with different ids have it.
pub fn by_ssh(&self, dest: &str) -> Result<Option<&str>, Vec<String>>;
```

- [ ] **Step 1: Write the failing tests**

Unit test in `hosts.rs` (build `Hosts` the way that file's tests already do): no match gives `Ok(None)`; one match gives its alias; two aliases with the same `ssh` and the same id give the lexicographically first; the same `ssh` with different ids gives `Err` with both names.

End-to-end, appended to `ip_address_is_saved_under_the_server_name` before its Review Focus 5 part:

```rust
    let calls = w.ssh_calls();
    std::fs::write(w.src.path().join("a.txt"), "again\n").unwrap();
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &format!("192.168.1.5:{}", dest.display())]));
    assert!(err.contains("192.168.1.5 is saved as myserver"), "{err}");
    assert_eq!(w.ssh_calls(), calls, "found by its ssh destination");
    assert_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "again\n");
```

And one new test, `two_hosts_with_one_ssh_destination_are_refused`: write a client config with `[hosts.a]` and `[hosts.b]`, different valid ids, both `ssh = "192.168.1.5"`; `irsync -a SRC 192.168.1.5:dest` fails, stderr names `a` and `b`, and `w.ssh_calls()` is 0.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --test onboard_e2e ip_address two_hosts`
Expected: FAIL.

- [ ] **Step 3: Implement** `by_ssh`. In `connect_for`, when `host` is neither a saved alias nor an EndpointId, call `config.hosts.by_ssh(host)`: on `Ok(Some(alias))` print `irsync: {host} is saved as {alias}` through `log.eprintln` and carry on with `alias` as the host for everything that follows (including `--ssh`, `--no-remote-dht` and the later recheck); on `Err(names)` bail with a message that lists them and says to remove or change one `ssh` value; on `Ok(None)` go on to ssh setup as today.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p irsync --lib config::hosts && cargo test -q -p irsync --test onboard_e2e`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/config/hosts.rs crates/irsync/src/main.rs crates/irsync/tests/onboard_e2e.rs
git commit -m "Find a saved host by its ssh destination

Typing the address a host was first set up with now reaches the saved
entry instead of running ssh setup again."
```

---

### Task 5: The client sends its host name

**Files:**
- Modify: `Cargo.toml` and `crates/irsync/Cargo.toml` (nix feature `hostname`), `crates/irsync/src/ssh/client.rs`, `crates/irsync/src/ssh/onboard.rs`, `crates/irsync/src/cli/mod.rs`, `crates/irsync/src/main.rs`
- Test: unit tests in `ssh/client.rs`; `crates/irsync/tests/onboard_e2e.rs`

**Interfaces:**
- Consumes: `short_host_name` (Task 2).
- Produces:
  - `pub fn client_host_name() -> anyhow::Result<String>` in `ssh/client.rs`: `nix::unistd::gethostname()` through `short_host_name`. The error says the host name (shown with `{:?}`) cannot be used as a name on the server and to set a host name on this machine. In the builds where `IRSYNC_TEST_EUID_ROOT` is honoured (`main.rs`, `running_as_root`; gate it the same way), the env `IRSYNC_TEST_HOSTNAME` replaces the system's answer.
  - `pub fn sh_quote(s: &str) -> String` in `ssh/client.rs`: always single-quoted, `'` written as `'\''`.
  - `run_setup(rsh, dest, client, client_name: &str, recheck, no_dht)`: appends ` --client-name ` + `sh_quote(client_name)`.
  - CLI: `SshSetup { client, client_name: String (required, `--client-name`), recheck, no_dht }`. `run_ssh_setup` passes it to `setup::run`, whose new parameter Task 6 defines. **In this task `setup::run` accepts and ignores it.**
  - `onboard` calls `client_host_name()` itself, before the probe.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn shell_quoting() {
    assert_eq!(sh_quote("myclient"), "'myclient'");
    assert_eq!(sh_quote("my client"), "'my client'");
    assert_eq!(sh_quote("a'b"), r"'a'\''b'");
    assert_eq!(sh_quote("$(touch x)`y`"), "'$(touch x)`y`'");
}
```

End-to-end: `World` gets `client_name: String` (default `"myclient"`), set as `IRSYNC_TEST_HOSTNAME` in `irsync_bare`.

```rust
/// Review Focus 1: the name is data on the server's command line.
#[test]
fn client_name_with_shell_characters_runs_nothing() {
    let mut w = World::new();
    w.client_name = "x'; touch \"$HOME/pwned\"; echo '".into();
    let dest = w.server.path().join("dest");
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    assert!(!w.server.path().join("pwned").exists());
}

#[test]
fn unusable_client_name_stops_before_ssh() {
    let mut w = World::new();
    w.client_name = "localhost".into();
    let out = w.irsync(&["-a", &w.src_arg(), "myserver:dest"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("host name"));
    assert_eq!(w.ssh_calls(), 0);
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync shell_quoting`
Expected: FAIL to compile.

- [ ] **Step 3: Implement** the items in Interfaces. Reuse `sh_quote` for nothing else in this task.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p irsync --lib ssh && cargo test -q -p irsync --test onboard_e2e && cargo test -q -p irsync --test cli`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/irsync/Cargo.toml crates/irsync/src crates/irsync/tests/onboard_e2e.rs
git commit -m "Send the client's host name with ssh setup"
```

---

### Task 6: The server saves and grants the client's name

**Files:**
- Modify: `crates/irsync/src/ssh/setup.rs`, `crates/irsync/src/ssh/onboard.rs` (`summary` wording only)
- Test: unit tests in `setup.rs`; `crates/irsync/tests/onboard_e2e.rs`

**Interfaces:**
- Consumes: `--client-name` (Task 5); `manage::hosts::host_add`, `manage::grants::grant_immediate`, `PeerSpec::Alias`.
- Produces, in `ssh/setup.rs`:

```rust
/// Whether `name` may stand for `client` on this server: a valid alias
/// that is free or already points at `client`.
pub fn check_client_name(hosts: &Hosts, client: EndpointId, name: &str) -> anyhow::Result<()>;

/// Saves `[hosts.name]` for `client` and grants `name` immediate access.
/// False when the client was already granted, by name or by id.
pub fn grant_client(
    doc: &mut DocumentMut,
    hosts: &Hosts,
    client: EndpointId,
    name: &str,
) -> anyhow::Result<bool>;

pub fn run(paths: &Paths, client: EndpointId, client_name: &str,
           recheck: bool, no_dht: bool) -> anyhow::Result<SetupReport>;
```

  - `run` calls `check_client_name` first, before the identity step.
  - The "taken" error names the entry: `the name "myclient" is already used on this server for another client (<short id>); remove [hosts.myclient] from <config path> there, or rename this machine`.
  - `grant_client` adds `[hosts.name]` with only `endpoint_id` when the entry is missing, also when the grant itself is already there by bare id (that entry is left as it is). The resolve closure it gives `grant_immediate` knows `name -> client` as well as the loaded hosts.
  - With `recheck` and a missing grant (`GrantOutcome::Missing`), nothing is saved, the host entry included, as today.

- [ ] **Step 1: Write the failing unit tests** in `setup.rs`. Build `Hosts` and the document from config text, the way `crates/irsync/tests/manage_hosts.rs` and `manage_grants.rs` do.

```rust
#[test]
fn client_name_rules() {
    let hosts = hosts_from("[hosts.myclient]\nendpoint_id = \"ID1\"\n"); // id(1)
    assert!(check_client_name(&hosts, id(1), "myclient").is_ok());
    assert!(check_client_name(&hosts, id(2), "other").is_ok());
    let e = check_client_name(&hosts, id(2), "myclient").unwrap_err().to_string();
    assert!(e.contains("[hosts.myclient]"), "{e}");
    for bad in ["", "all", "*", "a/b", "a:b"] {
        assert!(check_client_name(&hosts, id(2), bad).is_err(), "{bad:?}");
    }
}

#[test]
fn grants_the_name() {
    let mut doc = DocumentMut::new();
    assert!(grant_client(&mut doc, &Hosts::default(), id(1), "myclient").unwrap());
    let t: toml::Table = doc.to_string().parse().unwrap();
    assert_eq!(t["hosts"]["myclient"]["endpoint_id"].as_str(), Some(&*id(1).to_string()));
    assert_eq!(t["immediate"]["rw"].as_array().unwrap(), &[toml::Value::from("myclient")]);
}

/// Review Focus 2.
#[test]
fn names_that_need_quoting_round_trip() {
    for name in ["my client", "say \"hi\"", "tab\there", "ü", "a.b"] {
        let mut doc = DocumentMut::new();
        grant_client(&mut doc, &Hosts::default(), id(1), name).unwrap();
        let t: toml::Table = doc.to_string().parse().unwrap();
        assert!(t["hosts"].as_table().unwrap().contains_key(name), "{name:?}: {doc}");
        assert_eq!(t["immediate"]["rw"][0].as_str(), Some(name));
    }
}

#[test]
fn a_bare_id_grant_counts() {
    let mut doc: DocumentMut = format!("[immediate]\nrw = [\"{}\"]\n", id(1)).parse().unwrap();
    assert!(!grant_client(&mut doc, &Hosts::default(), id(1), "myclient").unwrap());
    let t: toml::Table = doc.to_string().parse().unwrap();
    assert_eq!(t["immediate"]["rw"][0].as_str(), Some(&*id(1).to_string()));
    assert!(t["hosts"].as_table().unwrap().contains_key("myclient"));
}
```

If `Hosts` has no `Default`, use that file's existing way to make an empty one. `"a.b"` is passed straight to `grant_client` here (the client cuts at `.`, the server does not rely on that).

- [ ] **Step 2: Write the failing end-to-end tests**

Change the helper `immediate_granted(w, id)` to resolve names: an `rw` entry counts when it is `id` or when `[hosts.<entry>].endpoint_id` in the server config is `id`. Then:

```rust
#[test]
fn server_knows_the_client_by_name() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    let t: toml::Table = w.server_toml().parse().unwrap();
    assert_eq!(t["hosts"]["myclient"]["endpoint_id"].as_str(),
               Some(&*client_id(w.client.path())));
    assert_eq!(t["immediate"]["rw"][0].as_str(), Some("myclient"));
}

#[test]
fn a_taken_client_name_is_refused_and_changes_nothing() {
    let w = World::new();
    // Another client already has the name. Use any other valid id.
    w.server_config("", &format!("[hosts.myclient]\nendpoint_id = \"{OTHER_ID}\"\n"));
    let before = w.server_toml();
    let out = w.irsync(&["-a", &w.src_arg(), "myserver:dest"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("[hosts.myclient]"), "{err}");
    assert_eq!(w.server_toml(), before);
    assert!(probe(&w.server.path().join("xdg/irsync")).is_none(), "no daemon started");
    assert_eq!(w.client_config(), "", "no alias saved");
}
```

`OTHER_ID`: generate it in the test from `iroh::SecretKey::from_bytes(&[9; 32]).public()`.

- [ ] **Step 3: Run them and see them fail**

Run: `cargo test -q -p irsync --lib ssh::setup`
Expected: FAIL to compile.

- [ ] **Step 4: Implement** the items in Interfaces. In `summary`, the "granted this client" line shows the name and the short id.

- [ ] **Step 5: Run the tests**

Run: `cargo test -q -p irsync --lib ssh && cargo test -q -p irsync --test onboard_e2e`
Expected: PASS, including the older tests that use `immediate_granted`.

- [ ] **Step 6: Commit**

```bash
git add crates/irsync/src/ssh crates/irsync/tests/onboard_e2e.rs
git commit -m "Grant immediate access to the client's host name

ssh-setup saves the client under its host name in [hosts] and grants
that name. It refuses, before changing anything, a name that is not a
valid alias or that belongs to another client."
```

---

### Task 7: Automatic exports use the name

**Files:**
- Modify: `crates/irsync/src/manage/exports.rs` (`add_auto_export`), `crates/irsync/src/authz.rs` (`FileAuthorizer::add_auto_export`)
- Test: `crates/irsync/tests/manage_exports.rs`, `crates/irsync/tests/authz_snapshot.rs`, `crates/irsync/tests/onboard_e2e.rs`

**Interfaces:**
- Produces: `add_auto_export(doc: &mut DocumentMut, path: &Path, peer: EndpointId, alias: Option<&str>) -> Result<String, ManageError>`: `rw` holds `alias` when given, else the id. `FileAuthorizer` passes `Config::load(&self.paths)?.hosts.alias_of(&peer)`, read while it holds the config lock. The `Authorizer` trait does not change.

- [ ] **Step 1: Write the failing tests**

In `manage_exports.rs`:

```rust
#[test]
fn auto_export_grants_the_alias_when_there_is_one() {
    let mut doc = toml_edit::DocumentMut::new();
    add_auto_export(&mut doc, Path::new("/srv/a"), id(1), Some("my client")).unwrap();
    add_auto_export(&mut doc, Path::new("/srv/b"), id(1), None).unwrap();
    let t: toml::Table = doc.to_string().parse().unwrap();
    assert_eq!(t["exports"]["a"]["rw"][0].as_str(), Some("my client"));
    assert_eq!(t["exports"]["b"]["rw"][0].as_str(), Some(&*id(1).to_string()));
}
```

In `onboard_e2e.rs`, at the end of `server_knows_the_client_by_name`:

```rust
    let t: toml::Table = w.server_toml().parse().unwrap();
    let exports = t["exports"].as_table().unwrap();
    assert_eq!(exports.len(), 1);
    let (_, e) = exports.iter().next().unwrap();
    assert_eq!(e["rw"][0].as_str(), Some("myclient"));
```

Review Focus 3, in `authz_snapshot.rs`, in that file's style: a config with an export whose `rw` is `["myclient"]` and **no** `[hosts.myclient]`. First find out what loading it does today (an error, or an entry that matches nobody) by running the test with a placeholder assertion, then pin exactly that behaviour, and assert that a peer with any id gets no grant from that export. Name the test `export_naming_a_removed_host_grants_nobody`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --test manage_exports`
Expected: FAIL to compile (arity).

- [ ] **Step 3: Implement** the new parameter and the lookup in `FileAuthorizer`. Update the other callers and tests of `add_auto_export` to pass `None`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/manage/exports.rs crates/irsync/src/authz.rs crates/irsync/tests
git commit -m "Name the client in automatic exports when the server knows it"
```

---

### Task 8: Book pages and the full check

**Files:**
- Modify: `book/src/tutorial/first-sync.md`, `book/src/how-to/ssh-setup.md`, `book/src/reference/remote-addresses.md`, `book/src/reference/configuration.md`, `book/src/reference/management.md`

Check `tools/build-man.sh` first: if any of these five chapters is a man page source, also run `python3 -m unittest discover -s tools/tests`.

- [ ] **Step 1: Update the pages.** Each fact below goes on the page that already covers its subject; read the page first and match its wording and example style (`myclient`, `myserver`, `~/...`).

  - `remote-addresses.md`: `HOST` may be `[ipv6]` in brackets; a host that is not an alias or id is looked up by the `ssh` value of saved hosts before ssh setup runs; two saved hosts with one `ssh` value and different ids are refused.
  - `ssh-setup.md`: which aliases a first sync saves (the table in Task 3, in prose); the server is asked for its host name; the client sends its own short host name; the server saves it in `[hosts]` and grants the name; when setup is refused over the name, and how to fix it (remove `[hosts.NAME]` on the server, or rename the client); a client with a new key and the same name hits exactly this.
  - `first-sync.md`: output samples and config snippets that show a bare id on the server now show `myclient`.
  - `configuration.md`: `[hosts]` on a server now also holds clients added by ssh setup; `[immediate].rw` and export `rw` examples show a name.
  - `management.md`: `grant list` and export listings show the client's name; bare ids from before stay as they are.

- [ ] **Step 2: Run the full check**

Run: `tools/ci.sh check`
Expected: every step passes.

- [ ] **Step 3: Commit**

```bash
git add book/src
git commit -m "Document the aliases a first sync saves"
```
