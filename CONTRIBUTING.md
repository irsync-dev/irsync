# Contributing to irsync

Thanks for helping. The full guide is in the book, under
[Contributing](https://irsync.org/book/contributing/setup.html). This page is
the short version.

## Report a bug

Open an issue at
[github.com/irsync-dev/irsync/issues](https://github.com/irsync-dev/irsync/issues).
Say which irsync version (`irsync --version`) and which OS each end runs, give
the command, and paste what it printed. A run with `-v` helps.

## Set up a checkout

```sh
git clone https://github.com/irsync-dev/irsync
cd irsync
cargo build --workspace
git config core.hooksPath .githooks   # checks formatting before each push
```

Some tests compare irsync with rsync and need GNU rsync 3.1 or newer on your
`PATH`. On macOS, install it with `brew install rsync`. See
[Requirements](https://irsync.org/book/contributing/setup.html#requirements).

## Before you open a pull request

Run the same checks as CI:

```sh
cargo test -q --workspace --all-features --no-fail-fast
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
```

More on the tests, the stress tools and the design records is in
[Testing](https://irsync.org/book/contributing/testing.html) and the other
Contributing pages of the book.

## License

By contributing, you agree that your work is licensed under the
[MIT License](LICENSE), like the rest of irsync.
