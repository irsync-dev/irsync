use irsync::cli::FilterRule;
use irsync::filter::{FilterSet, Verdict};
use irsync::fs::paths::RelPath;

fn set(rules: &[FilterRule]) -> FilterSet {
    FilterSet::from_rules(rules).unwrap()
}
fn v(s: &FilterSet, p: &str, dir: bool) -> Verdict {
    s.matches(&RelPath::new(p.as_bytes()).unwrap(), dir)
}
fn ex(p: &str) -> FilterRule {
    FilterRule::Exclude(p.into())
}
fn inc(p: &str) -> FilterRule {
    FilterRule::Include(p.into())
}

#[test]
fn anchored_pattern_only_matches_root() {
    let s = set(&[ex("/foo")]);
    assert_eq!(v(&s, "foo", false), Verdict::Exclude);
    assert_eq!(v(&s, "a/foo", false), Verdict::Include);
    let s = set(&[ex("foo")]);
    assert_eq!(v(&s, "a/b/foo", false), Verdict::Exclude);
}

#[test]
fn dir_only_pattern_skips_files_named_same() {
    let s = set(&[ex("build/")]);
    assert_eq!(v(&s, "build", true), Verdict::Exclude);
    assert_eq!(v(&s, "x/build", true), Verdict::Exclude);
    assert_eq!(v(&s, "build", false), Verdict::Include);
}

#[test]
fn star_does_not_cross_slash_doublestar_does() {
    let s = set(&[ex("a/*.o")]);
    assert_eq!(v(&s, "a/x.o", false), Verdict::Exclude);
    assert_eq!(v(&s, "a/b/x.o", false), Verdict::Include);
    let s = set(&[ex("a/**.o")]);
    assert_eq!(v(&s, "a/b/x.o", false), Verdict::Exclude);
    let s = set(&[ex("/a/**")]);
    assert_eq!(v(&s, "a/b/c", false), Verdict::Exclude);
    assert_eq!(v(&s, "z/a/b", false), Verdict::Include);
}

#[test]
fn first_match_wins_include_then_exclude() {
    let s = set(&[inc("keep.txt"), ex("*.txt")]);
    assert_eq!(v(&s, "d/keep.txt", false), Verdict::Include);
    assert_eq!(v(&s, "d/other.txt", false), Verdict::Exclude);
    assert_eq!(v(&s, "d/other.rs", false), Verdict::Include);
}

#[test]
fn classes_question_escape_and_trailing_components() {
    let s = set(&[ex("[a-c]?.x"), ex("\\*lit"), ex("[!0-9]z")]);
    assert_eq!(v(&s, "bq.x", false), Verdict::Exclude);
    assert_eq!(v(&s, "dq.x", false), Verdict::Include);
    assert_eq!(v(&s, "*lit", false), Verdict::Exclude);
    assert_eq!(v(&s, "xlit", false), Verdict::Include);
    assert_eq!(v(&s, "az", false), Verdict::Exclude);
    assert_eq!(v(&s, "1z", false), Verdict::Include);
    let s = set(&[ex("b/c")]);
    assert_eq!(v(&s, "a/b/c", false), Verdict::Exclude);
    assert_eq!(v(&s, "a/xb/c", false), Verdict::Include);
}

#[test]
fn non_utf8_and_root() {
    let s = set(&[ex("*")]);
    let p = RelPath::new(b"\xff\xfe").unwrap();
    assert_eq!(s.matches(&p, false), Verdict::Exclude);
    assert_eq!(s.matches(&RelPath::root(), true), Verdict::Include);
}

#[test]
fn from_files_skip_comments() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("pats");
    std::fs::write(&f, "# c\n; c2\n\n*.log\n").unwrap();
    let s = set(&[FilterRule::ExcludeFrom(f.clone())]);
    assert_eq!(v(&s, "a.log", false), Verdict::Exclude);
    assert_eq!(v(&s, "# c", false), Verdict::Include);
    assert!(FilterSet::from_rules(&[FilterRule::IncludeFrom(d.path().join("nope"))]).is_err());
}

/// rsync: a pattern with `**` is matched against the full path, like one
/// with a slash, even when it has no slash.
#[test]
fn doublestar_without_slash_matches_the_full_path() {
    let s = set(&[ex("a**z")]);
    assert_eq!(v(&s, "a/b/z", false), Verdict::Exclude);
    assert_eq!(v(&s, "x/a/b/z", false), Verdict::Exclude);
    assert_eq!(v(&s, "x/ab/z", false), Verdict::Exclude);
    assert_eq!(v(&s, "xa/b/z", false), Verdict::Include);
    let s = set(&[ex("**.o")]);
    assert_eq!(v(&s, "d/x.o", false), Verdict::Exclude);
}

/// A pattern file is read as bytes: a non-UTF-8 name in it matches.
#[test]
fn from_files_keep_non_utf8_bytes() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("pats");
    std::fs::write(&f, b"caf\xe9.txt\n").unwrap();
    let s = set(&[FilterRule::ExcludeFrom(f)]);
    let p = RelPath::new(b"d/caf\xe9.txt").unwrap();
    assert_eq!(s.matches(&p, false), Verdict::Exclude);
    assert_eq!(s.rules()[0].pattern(), b"caf\xe9.txt");
}

/// Many stars on a long repetitive name: the work must grow with pattern
/// length times path length, not multiply with every star.
#[test]
fn many_stars_do_not_take_exponential_time() {
    let name = format!("{}.jpg", "a".repeat(200));
    let deep = format!("d/e/{name}");
    let started = std::time::Instant::now();
    for pat in [
        "*a*a*a*a*a*a*a*a*a*a*a*a*b",
        "*a*a*a*a*a*a*a*a*a*a*a*a?b",
        "**a**a**a**a**a**a**a**a**a**a**a**a**b",
        "/**a**a**a**a**a**a**a**a**a**a**a**a**b",
    ] {
        let s = set(&[ex(pat)]);
        assert_eq!(v(&s, &name, false), Verdict::Include, "{pat}");
        assert_eq!(v(&s, &deep, false), Verdict::Include, "{pat}");
    }
    let s = set(&[ex("*a*a*a*a*a*a*a*a*a*a*a*a*.jpg")]);
    assert_eq!(v(&s, &name, false), Verdict::Exclude);
    assert_eq!(v(&s, &deep, false), Verdict::Exclude);
    let s = set(&[ex("d/**a**a**a**a**a**a**a**a**.jpg")]);
    assert_eq!(v(&s, &deep, false), Verdict::Exclude);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// A session's rules are limited in size and number: the daemon compiles
/// what a client sends, so neither side takes more.
#[test]
fn rules_over_the_limits_are_refused() {
    use irsync::filter::{MAX_PATTERN, MAX_PATTERN_BYTES, MAX_RULES};
    let fits = "x".repeat(MAX_PATTERN);
    assert!(FilterSet::from_rules(&[ex(&fits)]).is_ok());
    let err = FilterSet::from_rules(&[ex(&format!("{fits}x"))]).unwrap_err();
    assert!(err.to_string().contains("longer than the limit"), "{err}");

    let many: Vec<FilterRule> = (0..=MAX_RULES).map(|_| ex("a")).collect();
    assert!(FilterSet::from_rules(&many[..MAX_RULES]).is_ok());
    let err = FilterSet::from_rules(&many).unwrap_err();
    assert!(err.to_string().contains("filter rules"), "{err}");

    let big: Vec<FilterRule> = (0..=MAX_PATTERN_BYTES / MAX_PATTERN)
        .map(|_| ex(&fits))
        .collect();
    let err = FilterSet::from_rules(&big).unwrap_err();
    assert!(err.to_string().contains("together"), "{err}");
}
