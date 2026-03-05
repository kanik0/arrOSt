# Filesystem

ArrOSt exposes a small VFS facade with disk-backed and RAM-backed implementations.

## Backends

- `diskfs-v2`: preferred when storage backend is ready.
- `ramfs`: automatic fallback when storage is unavailable.

## Capabilities

- Hierarchical path resolution with real directories
- `.` / `..` handling in path walks
- Read file (`cat`)
- Write/overwrite file (`echo <text> > <file>`)
- Delete file
- Directory creation through the inode-based VFS layer
- Copy file
- Metadata sync/reload operations through shell commands
- Metadata-only journal replay on mount for `diskfs-v2`

## Limits

- Fixed-size `diskfs-v2` metadata layout: 256 inodes, 16 MiB virtio disk image, 512-byte blocks.
- Metadata journaling is redo-only; file data uses ordered writes and is not journaled.
- Intended for deterministic kernel bring-up and tooling support, not full POSIX compatibility.
- No file-descriptor syscalls or permission enforcement yet.

## User-visible shell commands

- `ls`
- `ls /bin`
- `/bin/ls`
- `cat <file>`
- `/bin/cat <file>`
- `echo <text> > <file>`
- `/bin/echo <text> > <file>`
- `fm list`
- `fm open <file>`
- `fm copy <src> <dst>`
- `fm delete <file>`
- `/bin/fm list`
- `/bin/fm open <file>`
- `/bin/fm copy <src> <dst>`
- `/bin/fm delete <file>`
- `/bin/doom [status|play|run|stop]`
- `/bin/terminal`
- `sync`
- `reload`

## Relevant files

- `kernel/src/fs/mod.rs`
- `kernel/src/fs/diskfs_v2.rs`
- `kernel/src/fs/journal.rs`
- `kernel/src/fs/ramfs.rs`
- `kernel/src/shell.rs`
