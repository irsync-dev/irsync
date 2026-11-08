# filegen

`filegen` simulates the use of a POSIX filesystem: from a 64-bit seed it
applies a reproducible sequence of filesystem operations (create, append,
truncate, rename, symlink, hard link, chmod, set mtime, punch a hole, remove
a subtree, and more) to a directory tree, logging and timing every step, and
can resume a run from its saved state as if it had never stopped. It is the
mutating half of irsync's stress loop and shares no code with irsync or
fileset. Usage, the log and state formats, the configuration and its
defaults are in
[Generating test trees with filegen](https://irsync.org/book/contributing/filegen.html).
