# Storage

ArrOSt currently uses a virtio block backend on QEMU for persistent sector I/O.

## Backend

- Primary backend: `virtio-blk-legacy`
- `x86_64`: legacy PCI transport
- `aarch64`: virtio-mmio discovery wrapped behind a synthetic legacy I/O port window for the shared storage driver
- Sector size: `512` bytes
- Queue-based request/response path

## Responsibilities

- Discover compatible virtio block transport for the active architecture.
- On `aarch64`, map the discovered virtio-mmio slot into the shared legacy register layout expected by the block driver.
- Negotiate queue and transport state.
- Submit synchronous sector read/write requests.
- Expose device capacity and backend health in boot diagnostics.

## Runtime interface

Storage initialization report includes:

- backend name
- ready flag
- PCI location (`x86_64`) or zeroed PCI coordinates with synthetic legacy I/O base (`aarch64`)
- I/O base
- total sectors and bytes

## Block cache (M29)

- 256-entry LRU write-back sector cache (`kernel/src/storage/cache.rs`).
- Sits between the filesystem and the virtio-blk driver, caching all sector reads and writes transparently.
- Write-back policy: dirty entries are deferred until `sync`, cache eviction, or explicit `cache clear`.
- Initialized after `storage::init()`, before `fs::init()` on both architectures.
- Shell commands: `cache` (stats), `cache clear` (flush + invalidate), `cache sync` (flush only).
- `/proc/cache` exposes hit/miss/writeback statistics.

## Limits

- QEMU/virtio focused implementation.
- `diskfs-v2` now provides its own redo journal above sector I/O, with `Ordered` default mode and optional `Full` data journaling mode.
- No multi-device scheduling yet.

## Relevant files

- `kernel/src/storage/mod.rs`
- `kernel/src/storage/cache.rs`
- `kernel/src/arch/x86_64/port.rs`
- `kernel/src/arch/aarch64/port.rs`
- `scripts/qemu.sh`
- `scripts/qemu-aarch64.sh`
