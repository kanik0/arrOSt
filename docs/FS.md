# Filesystem

ArrOSt exposes a small VFS facade with disk-backed and RAM-backed implementations.

## Backends

- `diskfs-v0`: preferred when storage backend is ready.
- `ramfs`: automatic fallback when storage is unavailable.

## Capabilities

- Flat file listing (`ls`)
- Read file (`cat`)
- Write/overwrite file (`echo <text> > <file>`)
- Delete file
- Copy file
- Sync/reload operations through shell commands

## Limits

- Flat namespace (no hierarchical directories).
- Fixed file/table limits defined by backend constants.
- Intended for deterministic kernel bring-up and tooling support, not full POSIX compatibility.
- Slash-separated names are stored as flat keys (no real directory metadata).
- Built-in `/bin/*` command entries are persisted as regular filesystem records (`bin/<name>`).

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
- `kernel/src/fs/diskfs.rs`
- `kernel/src/fs/ramfs.rs`
- `kernel/src/shell.rs`
