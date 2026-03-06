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

## Limits

- QEMU/virtio focused implementation.
- No storage-device cache or journal below the filesystem layer.
- `diskfs-v2` now provides its own metadata-only redo journal above sector I/O.
- No multi-device scheduling yet.

## Relevant files

- `kernel/src/storage/mod.rs`
- `kernel/src/arch/x86_64/port.rs`
- `kernel/src/arch/aarch64/port.rs`
- `scripts/qemu.sh`
- `scripts/qemu-aarch64.sh`
