# Filesystem

ArrOSt exposes a small mount-aware VFS facade with disk-backed, RAM-backed, and synthetic filesystems.

## Backends

- `/`: `diskfs-v2` when storage is ready, otherwise `ramfs`.
- `/proc`: synthetic read-only `procfs`.
- `/tmp`: volatile `tmpfs`.

## Capabilities

- Hierarchical path resolution with real directories
- `.` / `..` handling in path walks
- Path resolution across `/`, `/proc`, and `/tmp` mount boundaries
- Per-process file-descriptor tables with `fd 0-2` reserved for serial stdin/stdout/stderr
- Read file (`cat`)
- Write/overwrite file (`echo <text> > <file>`)
- Delete file
- Directory creation through the inode-based VFS layer
- Copy file
- Directory-aware listing (`ls <path>`)
- Metadata sync/reload operations through shell commands
- Metadata-only journal replay on mount for `diskfs-v2`
- Synthetic process/runtime inspection through `/proc/self/pid`, `/proc/mounts`, `/proc/uptime`
- Syscall-facing file handles for `open/close/fread/fwrite/seek/fstat/dup/dup2`

## Limits

- Fixed-size `diskfs-v2` metadata layout: 256 inodes, 16 MiB virtio disk image, 512-byte blocks.
- Metadata journaling is redo-only; file data uses ordered writes and is not journaled.
- Intended for deterministic kernel bring-up and tooling support, not full POSIX compatibility.
- Filesystem descriptors are separate from the current UDP socket syscall namespace.
- `procfs` is read-only and currently exposes a small fixed entry set.
- `tmpfs` is independent from the root filesystem and is not persisted across boots.

## User-visible shell commands

- `ls`
- `ls /proc`
- `ls /tmp`
- `ls /bin`
- `/bin/ls`
- `/bin/ls /proc`
- `cat <file>`
- `cat /proc/self/pid`
- `/bin/cat <file>`
- `echo <text> > <file>`
- `echo <text> > /tmp/<file>`
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
- `kernel/src/fs/mount.rs`
- `kernel/src/fs/diskfs_v2.rs`
- `kernel/src/fs/journal.rs`
- `kernel/src/fs/procfs.rs`
- `kernel/src/fs/ramfs.rs`
- `kernel/src/fs/tmpfs.rs`
- `kernel/src/shell.rs`
