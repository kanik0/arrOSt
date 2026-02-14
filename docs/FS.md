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

## User-visible shell commands

- `ls`
- `cat <file>`
- `echo <text> > <file>`
- `fm list`
- `fm open <file>`
- `fm copy <src> <dst>`
- `fm delete <file>`
- `sync`
- `reload`

## Relevant files

- `kernel/src/fs/mod.rs`
- `kernel/src/fs/diskfs.rs`
- `kernel/src/fs/ramfs.rs`
- `kernel/src/shell.rs`
