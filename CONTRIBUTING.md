# Contributing to irsync

Thanks for helping. The full guide is in the book, under
[Contributing](https://irsync.org/dev/contributing/setup.html). This page is
the short version.

## Report a bug

Open an issue at
[github.com/irsync-dev/irsync/issues](https://github.com/irsync-dev/irsync/issues).
Say which irsync version (`irsync --version`) and which OS each end runs, give
the command, and paste what it printed. A run with `-v` helps.

For a security problem, email security@irsync.org instead of opening an
issue; see [SECURITY.md](SECURITY.md).

## Set up a checkout

```sh
git clone https://github.com/irsync-dev/irsync
cd irsync
cargo build --workspace
git config core.hooksPath .githooks   # checks formatting before each push
```

Some tests compare irsync with rsync and need GNU rsync 3.1 or newer on your
`PATH`. On macOS, install it with `brew install rsync`. See
[Requirements](https://irsync.org/dev/contributing/setup.html#requirements).

## Before you open a pull request

Run the same checks as CI:

```sh
tools/ci.sh check
```

It checks formatting, runs clippy and runs every test, including five slow
network-fault tests that take several minutes. It needs cargo-nextest, which
`tools/dev-setup.sh` installs. While you work on a change, this is quicker:

```sh
cargo test -q --workspace --all-features --no-fail-fast
```

More on the tests, the stress tools and the design records is in
[Testing](https://irsync.org/dev/contributing/testing.html) and the other
Contributing pages of the book.

## License

By contributing, you agree that your work is licensed under the
[MIT License](LICENSE), like the rest of irsync.
