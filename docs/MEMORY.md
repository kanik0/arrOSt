# Memory Subsystem

ArrOSt memory initialization sets up early allocator state and memory diagnostics during boot.

## Responsibilities

- Collect and report memory map statistics for the active architecture.
- Initialize a fixed kernel heap allocator for early runtime needs.
- Provide virtual/physical translation helpers used by virtio drivers.

## Current implementation

`kernel/src/mem/mod.rs` provides:

- `mem::init(&BootInfo) -> Result<MemoryInitReport, MemoryError>`
- `mem::init_without_boot_info_with_uefi_map(Option<UefiMemoryMapHandoff>) -> Result<MemoryInitReport, MemoryError>` (`aarch64`)
- `mem::virt_to_phys(virt_addr)`
- `mem::phys_to_virt(phys_addr)`

Architecture notes:

- `x86_64`: uses bootloader-provided memory regions (`BootInfo`) for stats and mapping metadata.
- `aarch64`: receives optional UEFI memory-map handoff from the UEFI loader after `ExitBootServices`; if present it is used for stats and translation hints, otherwise a conservative heap-only fallback report is emitted.

Heap allocator:

- Bump-style global allocator for current kernel scope.
- Fixed-size static heap.
- Allocation smoke test executed at boot and reported on serial.

## Safety notes

- Unsafe code is concentrated in allocator and address-translation sections.
- Unsafe invariants are documented inline (boot metadata assumptions, descriptor bounds checks).

## Limits

- No per-process address spaces yet.
- No advanced allocator strategy (fragmentation-aware allocator is not implemented yet).
- No demand paging or swap.

## Relevant files

- `kernel/src/mem/mod.rs`
- `kernel/src/main.rs`
