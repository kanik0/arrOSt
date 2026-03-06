# Filesystem

ArrOSt exposes a small mount-aware VFS facade with disk-backed, RAM-backed, and synthetic filesystems.

## Backends

- `/`: `diskfs-v2` when storage is ready, otherwise `ramfs`.
- `/proc`: synthetic read-only `procfs`.
- `/tmp`: volatile `tmpfs` with world-writable root (`0777`) for user scratch files.

## Capabilities

- Hierarchical path resolution with real directories
- `.` / `..` handling in path walks
- Path resolution across `/`, `/proc`, and `/tmp` mount boundaries
- Per-process file-descriptor tables with `fd 0-2` reserved for serial stdin/stdout/stderr
- Read file (`cat`)
- Write/overwrite file (`echo <text> > <file>`)
- Delete file
- Directory creation through the inode-based VFS layer
- Working-directory aware path resolution (`pwd`, `cd`, relative `ls/cat/echo/link/symlink/fm`)
- Hard links (`link <src> <dst>`)
- Symbolic links (`symlink <target> <linkpath>`)
- Symlink resolution across mount-aware path walks with `ELOOP` guard after 8 hops
- Copy file
- Rename/move (`mv <src> <dst>`)
- Directory-aware listing (`ls <path>`)
- Metadata inspection (`stat <path>`)
- Mode changes (`chmod <mode> <path>`)
- Inode metadata tracking (`uid`, `gid`, `mode`, `nlink`, `atime`, `mtime`, `ctime`)
- Permission enforcement for filesystem opens/listing/reads/writes against inode mode bits
- Metadata sync/reload operations through shell commands
- Metadata-only journal replay on mount for `diskfs-v2`
- Dentry cache for repeated mount-aware path resolution with conservative invalidation on namespace mutations
- Automatic shell/terminal dispatch from bare commands (`ls`, `cat`, `ps`, `link`, `symlink`, `fm`, ...) to `/bin/<cmd>` when the file exists
- Synthetic process/runtime inspection through `/proc/self/pid`, `/proc/mounts`, `/proc/uptime`
- Syscall-facing file handles for `open/close/fread/fwrite/seek/fstat/dup/dup2`

## Limits

- Fixed-size `diskfs-v2` metadata layout: 256 inodes, 16 MiB virtio disk image, 512-byte blocks.
- Metadata journaling is redo-only; file data uses ordered writes and is not journaled.
- Intended for deterministic kernel bring-up and tooling support, not full POSIX compatibility.
- Filesystem descriptors are separate from the current UDP socket syscall namespace.
- `procfs` is read-only and currently exposes a small fixed entry set.
- `tmpfs` is independent from the root filesystem and is not persisted across boots.
- Symlink resolution stops after 8 chained hops and returns `ELOOP`.
- Permission model is intentionally small: kernel/external shell paths are privileged, ring-3 processes are treated as `uid=1000 gid=1000`.
- Dentry caching is a performance hint only; it can be disabled at build time with `ARROST_DISABLE_DENTRY_CACHE=1`.

## User-visible shell commands

- `pwd`
- `cd <dir>`
- `ls`
- `ls /proc`
- `ls /tmp`
- `ls /bin`
- `/bin/ls`
- `/bin/ls /proc`
- `cat <file>`
- `cat /proc/self/pid`
- `cat /proc/mounts`
- `cat /proc/uptime`
- `/bin/cat <file>`
- `echo <text> > <file>`
- `echo <text> > /tmp/<file>`
- `stat <path>`
- `chmod <mode> <path>`
- `mkdir <dir>`
- `mv <src> <dst>`
- `link <src> <dst>`
- `symlink <target> <linkpath>`
- `/bin/echo <text> > <file>`
- `/bin/link <src> <dst>`
- `/bin/symlink <target> <linkpath>`
- `fm list`
- `fm list <path>`
- `fm cd <dir>`
- `fm open <file>`
- `fm copy <src> <dst>`
- `fm delete <file>`
- `/bin/fm list`
- `/bin/fm list <path>`
- `/bin/fm cd <dir>`
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
- `kernel/src/fs/fd.rs`
- `kernel/src/fs/procfs.rs`
- `kernel/src/fs/ramfs.rs`
- `kernel/src/fs/tmpfs.rs`
- `kernel/src/fs/dentry.rs`
- `kernel/src/shell.rs`
