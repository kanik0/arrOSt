# Memory Subsystem

ArrOSt memory initialization sets up early allocator state and memory diagnostics during boot.

## Responsibilities

- Collect and report memory map statistics for the active architecture.
- Initialize a fixed kernel heap allocator for early runtime needs.
- Provide virtual/physical translation helpers used by virtio drivers.
- Provide the translation primitives used by ring-3 copy boundaries (`copy_from_user` / `copy_to_user`).
- Support per-process ring-3 page-table ownership and user-page mapping.

## Current implementation

`kernel/src/mem/mod.rs` provides:

- `mem::init(&BootInfo) -> Result<MemoryInitReport, MemoryError>`
- `mem::init_without_boot_info_with_uefi_map(Option<UefiMemoryMapHandoff>) -> Result<MemoryInitReport, MemoryError>` (`aarch64`)
- `mem::virt_to_phys(virt_addr)`
- `mem::phys_to_virt(phys_addr)`

Architecture notes:

- `x86_64`: uses bootloader-provided memory regions (`BootInfo`) for stats and mapping metadata.
- `aarch64`: receives optional UEFI memory-map handoff from the UEFI loader after `ExitBootServices`; if present it is used for stats and translation hints, otherwise a conservative heap-only fallback report is emitted.

Ring-3 runtime notes:

- Ring-3 ELF processes now own a dedicated address-space root per process.
- User ELF segments and stacks are mapped into a dedicated user virtual range (current linker scripts place user code/data near `0x0000_2000_0000_0000`).
- Kernel/user copies translate user virtual addresses through the owning process page tables and then access the backing memory via kernel-visible physical aliases.
- Ring-3 page-table creation currently clones the active root table verbatim, then overlays user mappings plus a fixed trampoline page (`mem::TRAMPOLINE_VADDR`); tighter KPTI trimming is deferred until kernel runtime no longer depends on low virtual addresses across CR3/TTBR switches.

Heap allocator:

- Bump-style global allocator for current kernel scope.
- Fixed-size static heap.
- Allocation smoke test executed at boot and reported on serial.

## Safety notes

- Unsafe code is concentrated in allocator and address-translation sections.
- Unsafe invariants are documented inline (boot metadata assumptions, descriptor bounds checks).

## Limits

- No advanced allocator strategy (fragmentation-aware allocator is not implemented yet).
- No demand paging or swap.
- No copy-on-write, lazy allocation, or demand-faulted user-page population.

## Relevant files

- `kernel/src/mem/mod.rs`
- `kernel/src/proc/ring3_groundwork.rs`
- `kernel/src/main.rs`
