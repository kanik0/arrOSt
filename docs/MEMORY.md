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
- M11 KPTI transition wiring is complete: ring-3 page tables include a dedicated trampoline page (`mem::TRAMPOLINE_VADDR`), and syscall/fault transitions route through architecture trampoline entry/exit paths with per-CPU KPTI scratch-assisted CR3/TTBR switching. Ring-3 roots currently still clone the active kernel root table as the kernel executes from low virtual addresses during switches.
- M13 VMA tracking: `kernel/src/mem/vma.rs` provides `VmaEntry` / `VmaFlags` with `COW` and `ANON` bits. Each ring-3 process maintains a `vma_list` with up to 16 entries covering ELF segments, stack, and anonymous mappings.
- M13 CoW + demand paging: `fork` marks all writable pages read-only in both parent and child; a write fault triggers a private page copy (CoW). Anonymous VMAs (`mmap`/`brk`) are zero-filled on first access (demand paging). Physical frame reference counting is implemented via `Arc<UserPageHolder>` strong-count.

Heap allocator:

- Bump-style global allocator for current kernel scope.
- Fixed-size static heap.
- Allocation smoke test executed at boot and reported on serial.

## Safety notes

- Unsafe code is concentrated in allocator and address-translation sections.
- Unsafe invariants are documented inline (boot metadata assumptions, descriptor bounds checks).

## Limits

- No advanced allocator strategy (fragmentation-aware allocator is not implemented yet).
- No swap.
- Demand paging and copy-on-write are implemented for ring-3 anonymous VMAs (M13); file-backed VMAs are not yet supported.

## Relevant files

- `kernel/src/mem/mod.rs`
- `kernel/src/mem/vma.rs`
- `kernel/src/proc/ring3_groundwork.rs`
- `kernel/src/proc/mod.rs` (M13 CoW / demand-page fault handler)
- `kernel/src/main.rs`
