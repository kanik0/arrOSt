# Memory Subsystem

ArrOSt memory initialization configures paging, frame allocation, and kernel heap allocation during early boot.

## Responsibilities

- Collect and report boot memory map statistics.
- Create an offset page table mapper from bootloader-provided physical mapping.
- Allocate and map kernel heap pages.
- Enforce guard pages around heap region.
- Provide virtual/physical translation helpers used by virtio drivers.

## Current implementation

`kernel/src/mem/mod.rs` provides:

- `mem::init(&BootInfo) -> Result<MemoryInitReport, MemoryError>`
- `mem::virt_to_phys(virt_addr)`
- `mem::phys_to_virt(phys_addr)`

Heap allocator:

- Bump-style global allocator for current kernel scope.
- Fixed heap with low/high guard pages.
- Allocation smoke test executed at boot and reported on serial.

## Safety notes

- Unsafe code is concentrated in page-table and address-translation sections.
- Unsafe invariants are documented inline (bootloader mapping assumptions, mapper construction).

## Limits

- No per-process address spaces yet.
- No advanced allocator strategy (fragmentation-aware allocator is not implemented yet).
- No demand paging or swap.

## Relevant files

- `kernel/src/mem/mod.rs`
- `kernel/src/main.rs`
