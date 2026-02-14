# Storage

ArrOSt currently uses a virtio block backend on QEMU for persistent sector I/O.

## Backend

- Primary backend: `virtio-blk-legacy`
- Sector size: `512` bytes
- Queue-based request/response path

## Responsibilities

- Discover compatible PCI virtio block device.
- Negotiate queue and transport state.
- Submit synchronous sector read/write requests.
- Expose device capacity and backend health in boot diagnostics.

## Runtime interface

Storage initialization report includes:

- backend name
- ready flag
- PCI location
- I/O base
- total sectors and bytes

## Limits

- QEMU/virtio focused implementation.
- No advanced caching/journaling layer.
- No multi-device scheduling yet.

## Relevant files

- `kernel/src/storage/mod.rs`
- `scripts/qemu.sh`
