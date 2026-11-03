# Keep platform differences in one crate

Status: accepted on 2026-11-02.

Unix and Windows differences live in `pp-os`, and no other crate may name `libc` or `windows-sys`.

The current arrangement is per-crate `cfg(target_os)` blocks and direct `std::os::unix` imports, and its cost is already visible in CI: the Windows job excludes 8 of 15 crates by name, with the comment "Unix-only crates; remove each exclusion when its port ticket is fixed." The blockers span storage, source, core, server and desktop, and several are unconditional imports rather than guarded ones, so nothing stops the next one being added anywhere.

`pp-os` has two real adapters, which is what makes it a seam rather than an indirection. Its interface is small and behavioural: open a path without following symlinks, create a private directory, check that a file is owned by the current user, acquire the data-directory lock, get a shutdown signal, and restrict a path to its owner. On Unix that is `openat`, `fstatat`, `geteuid` and device-plus-inode identity. On Windows it is `CreateFileW` with `FILE_FLAG_OPEN_REPARSE_POINT`, owner SID and DACL checks, console control events, and advisory locking.

One thing this must not hide is that the data-directory lock is not a library call. On Unix the lock file itself is opened with `openat` and `O_NOFOLLOW` and its identity is checked with device and inode numbers before any lock is taken. A plan that says "use a cross-platform file lock" and stops there leaves the actual Windows blocker in place.

The rejected alternative is to keep per-crate `cfg` blocks and fix them one at a time. It is a smaller change per PR, but it guarantees the same problem returns with every new file-handling feature, and it leaves the platform rules unwritten. The second rejected alternative is to port only what Windows requires and leave the rest on Unix assumptions, which is what produced eight CI exclusions in the first place.

This crate is also the reason the Windows port is scheduled before the domain families rather than after them. Every later slice adds file handling, so porting late multiplies the work instead of deferring it.
