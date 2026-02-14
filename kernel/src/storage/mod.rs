// kernel/src/storage/mod.rs: M6 virtio-blk (legacy PCI) storage backend for QEMU.
use crate::arch::x86_64::port;
use crate::mem;
use crate::serial;
use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::mem::size_of;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering, fence};

pub const SECTOR_SIZE: usize = 512;
const MAX_QUEUE_SIZE: u16 = 256;
const MAX_QUEUE_SIZE_USIZE: usize = MAX_QUEUE_SIZE as usize;
const VRING_ALIGN: usize = 4096;
const MAX_POLL_SPINS: usize = 2_000_000;
const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_BLK_TRANSITIONAL_ID: u16 = 0x1001;
const VIRTIO_BLK_MODERN_ID: u16 = 0x1042;

const PCI_CONFIG_ADDR: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

const VIRTIO_PCI_HOST_FEATURES: u16 = 0x00;
const VIRTIO_PCI_GUEST_FEATURES: u16 = 0x04;
const VIRTIO_PCI_QUEUE_PFN: u16 = 0x08;
const VIRTIO_PCI_QUEUE_NUM: u16 = 0x0C;
const VIRTIO_PCI_QUEUE_SEL: u16 = 0x0E;
const VIRTIO_PCI_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_PCI_STATUS: u16 = 0x12;
const VIRTIO_PCI_ISR: u16 = 0x13;
const VIRTIO_PCI_DEVICE_CONFIG: u16 = 0x14;

const VIRTIO_STATUS_ACK: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FAILED: u8 = 128;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;

const fn align_up(value: usize, align: usize) -> usize {
    (value + (align - 1)) & !(align - 1)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqUsedElem {
    id: u32,
    len: u32,
}

#[repr(C)]
struct VirtqAvail {
    flags: u16,
    idx: u16,
    ring: [u16; MAX_QUEUE_SIZE_USIZE],
    used_event: u16,
}

#[repr(C)]
struct VirtqUsed {
    flags: u16,
    idx: u16,
    ring: [VirtqUsedElem; MAX_QUEUE_SIZE_USIZE],
    avail_event: u16,
}

const DESC_BYTES: usize = size_of::<VirtqDesc>() * MAX_QUEUE_SIZE_USIZE;
const AVAIL_BYTES: usize = size_of::<VirtqAvail>();
const USED_OFFSET: usize = align_up(DESC_BYTES + AVAIL_BYTES, VRING_ALIGN);
const USED_BYTES: usize = size_of::<VirtqUsed>();
const VRING_BYTES: usize = USED_OFFSET + USED_BYTES;

#[repr(C, align(4096))]
struct QueueMemory {
    bytes: [u8; VRING_BYTES],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioBlkReqHeader {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

#[repr(C, align(16))]
struct RequestMemory {
    header: VirtioBlkReqHeader,
    data: [u8; SECTOR_SIZE],
    status: u8,
    _pad: [u8; 15],
}

struct QueueMemoryCell(UnsafeCell<QueueMemory>);
struct RequestMemoryCell(UnsafeCell<RequestMemory>);

// SAFETY: access is serialized by `STORAGE_LOCK`.
unsafe impl Sync for QueueMemoryCell {}
// SAFETY: access is serialized by `STORAGE_LOCK`.
unsafe impl Sync for RequestMemoryCell {}

static QUEUE_MEMORY: QueueMemoryCell = QueueMemoryCell(UnsafeCell::new(QueueMemory {
    bytes: [0; VRING_BYTES],
}));

static REQUEST_MEMORY: RequestMemoryCell = RequestMemoryCell(UnsafeCell::new(RequestMemory {
    header: VirtioBlkReqHeader {
        req_type: 0,
        reserved: 0,
        sector: 0,
    },
    data: [0; SECTOR_SIZE],
    status: 0,
    _pad: [0; 15],
}));

#[derive(Clone, Copy)]
pub struct StorageInitReport {
    pub backend: &'static str,
    pub ready: bool,
    pub io_base: u16,
    pub pci_bus: u8,
    pub pci_device: u8,
    pub pci_function: u8,
    pub pci_device_id: u16,
    pub capacity_sectors: u64,
    pub capacity_bytes: u64,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum StorageError {
    NotReady,
    NotFound,
    QueueTooSmall,
    QueueUnavailable,
    AddressTranslationFailed,
    OutOfRange,
    IoTimeout,
    DeviceFailure,
}

impl StorageError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotReady => "not_ready",
            Self::NotFound => "not_found",
            Self::QueueTooSmall => "queue_too_small",
            Self::QueueUnavailable => "queue_unavailable",
            Self::AddressTranslationFailed => "address_translation_failed",
            Self::OutOfRange => "out_of_range",
            Self::IoTimeout => "io_timeout",
            Self::DeviceFailure => "device_failure",
        }
    }
}

struct StorageCell(UnsafeCell<StorageState>);

// SAFETY: access is serialized through `STORAGE_LOCK`.
unsafe impl Sync for StorageCell {}

static STORAGE_LOCK: SpinLock = SpinLock::new();
static STORAGE_STATE: StorageCell = StorageCell(UnsafeCell::new(StorageState::new()));

#[derive(Clone, Copy)]
struct PciLocation {
    bus: u8,
    device: u8,
    function: u8,
    device_id: u16,
    io_base: u16,
}

struct StorageState {
    initialized: bool,
    io_base: u16,
    pci_bus: u8,
    pci_device: u8,
    pci_function: u8,
    pci_device_id: u16,
    capacity_sectors: u64,
    queue_size: u16,
    last_used_idx: u16,
    ready: bool,
}

impl StorageState {
    const fn new() -> Self {
        Self {
            initialized: false,
            io_base: 0,
            pci_bus: 0,
            pci_device: 0,
            pci_function: 0,
            pci_device_id: 0,
            capacity_sectors: 0,
            queue_size: 0,
            last_used_idx: 0,
            ready: false,
        }
    }

    fn report(&self) -> StorageInitReport {
        StorageInitReport {
            backend: if self.ready {
                "virtio-blk-legacy"
            } else {
                "none"
            },
            ready: self.ready,
            io_base: self.io_base,
            pci_bus: self.pci_bus,
            pci_device: self.pci_device,
            pci_function: self.pci_function,
            pci_device_id: self.pci_device_id,
            capacity_sectors: self.capacity_sectors,
            capacity_bytes: self.capacity_sectors.saturating_mul(SECTOR_SIZE as u64),
        }
    }

    fn init(&mut self) -> StorageInitReport {
        if self.initialized {
            return self.report();
        }

        match self.try_init() {
            Ok(()) => {
                self.initialized = true;
            }
            Err(error) => {
                self.initialized = true;
                self.ready = false;
                serial::write_fmt(format_args!("Storage: init failed ({})\n", error.as_str()));
            }
        }

        self.report()
    }

    fn try_init(&mut self) -> Result<(), StorageError> {
        let Some(device) = find_virtio_blk_pci() else {
            return Err(StorageError::NotFound);
        };

        self.io_base = device.io_base;
        self.pci_bus = device.bus;
        self.pci_device = device.device;
        self.pci_function = device.function;
        self.pci_device_id = device.device_id;

        self.virtio_write_status(0);
        self.virtio_write_status(VIRTIO_STATUS_ACK);
        self.virtio_write_status(VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER);

        let host_features = self.virtio_read_u32(VIRTIO_PCI_HOST_FEATURES);
        self.virtio_write_u32(VIRTIO_PCI_GUEST_FEATURES, host_features);

        self.virtio_write_u16(VIRTIO_PCI_QUEUE_SEL, 0);
        let queue_size = self.virtio_read_u16(VIRTIO_PCI_QUEUE_NUM);
        if queue_size == 0 {
            self.virtio_write_status(VIRTIO_STATUS_FAILED);
            return Err(StorageError::QueueUnavailable);
        }
        if queue_size > MAX_QUEUE_SIZE {
            self.virtio_write_status(VIRTIO_STATUS_FAILED);
            return Err(StorageError::QueueTooSmall);
        }
        self.queue_size = queue_size;

        // SAFETY: serialized by `STORAGE_LOCK`; queue memory is dedicated to this driver.
        unsafe {
            (*QUEUE_MEMORY.0.get()).bytes.fill(0);
        }

        let queue_virt = queue_memory_base();
        let queue_phys =
            mem::virt_to_phys(queue_virt as usize).ok_or(StorageError::AddressTranslationFailed)?;
        if !queue_phys.is_multiple_of(VRING_ALIGN as u64) {
            self.virtio_write_status(VIRTIO_STATUS_FAILED);
            return Err(StorageError::AddressTranslationFailed);
        }

        self.virtio_write_u32(VIRTIO_PCI_QUEUE_PFN, (queue_phys >> 12) as u32);
        if self.virtio_read_u32(VIRTIO_PCI_QUEUE_PFN) == 0 {
            self.virtio_write_status(VIRTIO_STATUS_FAILED);
            return Err(StorageError::QueueUnavailable);
        }

        let cap_low = self.virtio_read_u32(VIRTIO_PCI_DEVICE_CONFIG);
        let cap_high = self.virtio_read_u32(VIRTIO_PCI_DEVICE_CONFIG + 4);
        self.capacity_sectors = ((cap_high as u64) << 32) | (cap_low as u64);

        self.last_used_idx = 0;
        self.ready = true;
        self.virtio_write_status(
            VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
        );
        Ok(())
    }

    fn read_sector(
        &mut self,
        sector: u64,
        out: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), StorageError> {
        if !self.ready {
            return Err(StorageError::NotReady);
        }
        if sector >= self.capacity_sectors {
            return Err(StorageError::OutOfRange);
        }
        self.submit_io(VIRTIO_BLK_T_IN, sector, Some(out))
    }

    fn write_sector(&mut self, sector: u64, data: &[u8; SECTOR_SIZE]) -> Result<(), StorageError> {
        if !self.ready {
            return Err(StorageError::NotReady);
        }
        if sector >= self.capacity_sectors {
            return Err(StorageError::OutOfRange);
        }
        let mut scratch = [0u8; SECTOR_SIZE];
        scratch.copy_from_slice(data);
        self.submit_io(VIRTIO_BLK_T_OUT, sector, Some(&mut scratch))
    }

    fn submit_io(
        &mut self,
        request_type: u32,
        sector: u64,
        data: Option<&mut [u8; SECTOR_SIZE]>,
    ) -> Result<(), StorageError> {
        let Some(data_buf) = data else {
            return Err(StorageError::DeviceFailure);
        };

        // SAFETY: serialized by `STORAGE_LOCK`; request memory is single-owner here.
        unsafe {
            let req = &mut *REQUEST_MEMORY.0.get();
            req.header.req_type = request_type;
            req.header.reserved = 0;
            req.header.sector = sector;
            req.status = 0xFF;
            if request_type == VIRTIO_BLK_T_OUT {
                req.data.copy_from_slice(data_buf);
            }
        }

        let header_phys = mem::virt_to_phys(request_header_ptr() as usize)
            .ok_or(StorageError::AddressTranslationFailed)?;
        let data_phys = mem::virt_to_phys(request_data_ptr() as usize)
            .ok_or(StorageError::AddressTranslationFailed)?;
        let status_phys = mem::virt_to_phys(request_status_ptr() as usize)
            .ok_or(StorageError::AddressTranslationFailed)?;

        // SAFETY: serialized by `STORAGE_LOCK`; queue memory is exclusively owned by this driver.
        unsafe {
            let desc = queue_desc_ptr();
            let avail = queue_avail_ptr();
            let used = queue_used_ptr();

            write_volatile(
                desc.add(0),
                VirtqDesc {
                    addr: header_phys,
                    len: size_of::<VirtioBlkReqHeader>() as u32,
                    flags: VIRTQ_DESC_F_NEXT,
                    next: 1,
                },
            );
            let data_flags = if request_type == VIRTIO_BLK_T_IN {
                VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE
            } else {
                VIRTQ_DESC_F_NEXT
            };
            write_volatile(
                desc.add(1),
                VirtqDesc {
                    addr: data_phys,
                    len: SECTOR_SIZE as u32,
                    flags: data_flags,
                    next: 2,
                },
            );
            write_volatile(
                desc.add(2),
                VirtqDesc {
                    addr: status_phys,
                    len: 1,
                    flags: VIRTQ_DESC_F_WRITE,
                    next: 0,
                },
            );

            let avail_idx = read_volatile(addr_of!((*avail).idx));
            let slot = (avail_idx % self.queue_size) as usize;
            write_volatile(addr_of_mut!((*avail).ring[slot]), 0);
            fence(Ordering::SeqCst);
            write_volatile(addr_of_mut!((*avail).idx), avail_idx.wrapping_add(1));
            fence(Ordering::SeqCst);

            self.virtio_write_u16(VIRTIO_PCI_QUEUE_NOTIFY, 0);

            let expected_used = self.last_used_idx.wrapping_add(1);
            let mut spins = 0usize;
            loop {
                let observed = read_volatile(addr_of!((*used).idx));
                if observed == expected_used {
                    break;
                }
                if spins >= MAX_POLL_SPINS {
                    let _ = self.virtio_read_u8(VIRTIO_PCI_ISR);
                    return Err(StorageError::IoTimeout);
                }
                spins = spins.saturating_add(1);
                spin_loop();
            }
            self.last_used_idx = expected_used;
            let used_slot = (expected_used.wrapping_sub(1) % self.queue_size) as usize;
            let _head_id = read_volatile(addr_of!((*used).ring[used_slot].id));
        }

        // SAFETY: serialized by `STORAGE_LOCK`; request memory belongs to this driver.
        let status = unsafe { (*REQUEST_MEMORY.0.get()).status };
        if status != 0 {
            return Err(StorageError::DeviceFailure);
        }

        if request_type == VIRTIO_BLK_T_IN {
            // SAFETY: serialized by `STORAGE_LOCK`; request data just filled by device.
            unsafe {
                let req = &*REQUEST_MEMORY.0.get();
                data_buf.copy_from_slice(&req.data);
            }
        }

        Ok(())
    }

    fn virtio_write_status(&self, status: u8) {
        self.virtio_write_u8(VIRTIO_PCI_STATUS, status);
    }

    fn virtio_read_u8(&self, offset: u16) -> u8 {
        // SAFETY: `io_base + offset` is a validated virtio legacy I/O port range.
        unsafe { port::inb(self.io_base.saturating_add(offset)) }
    }

    fn virtio_write_u8(&self, offset: u16, value: u8) {
        // SAFETY: `io_base + offset` is a validated virtio legacy I/O port range.
        unsafe { port::outb(self.io_base.saturating_add(offset), value) }
    }

    fn virtio_read_u16(&self, offset: u16) -> u16 {
        // SAFETY: `io_base + offset` is a validated virtio legacy I/O port range.
        unsafe { port::inw(self.io_base.saturating_add(offset)) }
    }

    fn virtio_write_u16(&self, offset: u16, value: u16) {
        // SAFETY: `io_base + offset` is a validated virtio legacy I/O port range.
        unsafe { port::outw(self.io_base.saturating_add(offset), value) }
    }

    fn virtio_read_u32(&self, offset: u16) -> u32 {
        // SAFETY: `io_base + offset` is a validated virtio legacy I/O port range.
        unsafe { port::inl(self.io_base.saturating_add(offset)) }
    }

    fn virtio_write_u32(&self, offset: u16, value: u32) {
        // SAFETY: `io_base + offset` is a validated virtio legacy I/O port range.
        unsafe { port::outl(self.io_base.saturating_add(offset), value) }
    }
}

pub fn init() -> StorageInitReport {
    with_storage_mut(|state| state.init())
}

pub fn is_ready() -> bool {
    with_storage(|state| state.ready)
}

pub fn capacity_sectors() -> u64 {
    with_storage(|state| state.capacity_sectors)
}

pub fn read_sector(sector: u64, out: &mut [u8; SECTOR_SIZE]) -> Result<(), StorageError> {
    with_storage_mut(|state| state.read_sector(sector, out))
}

pub fn write_sector(sector: u64, data: &[u8; SECTOR_SIZE]) -> Result<(), StorageError> {
    with_storage_mut(|state| state.write_sector(sector, data))
}

pub fn log_info() {
    let report = with_storage(|state| state.report());
    if report.ready {
        serial::write_fmt(format_args!(
            "disk: backend={} pci={:02x}:{:02x}.{} devid={:#06x} io={:#06x} sectors={} bytes={}\n",
            report.backend,
            report.pci_bus,
            report.pci_device,
            report.pci_function,
            report.pci_device_id,
            report.io_base,
            report.capacity_sectors,
            report.capacity_bytes
        ));
    } else {
        serial::write_line("disk: backend=none status=unavailable");
    }
}

fn with_storage<R>(f: impl FnOnce(&StorageState) -> R) -> R {
    let _guard = STORAGE_LOCK.lock();
    // SAFETY: `STORAGE_LOCK` serializes access to global storage state.
    unsafe { f(&*STORAGE_STATE.0.get()) }
}

fn with_storage_mut<R>(f: impl FnOnce(&mut StorageState) -> R) -> R {
    let _guard = STORAGE_LOCK.lock();
    // SAFETY: `STORAGE_LOCK` serializes mutable access to global storage state.
    unsafe { f(&mut *STORAGE_STATE.0.get()) }
}

fn queue_memory_base() -> *mut u8 {
    // SAFETY: caller ensures serialized access to queue memory.
    unsafe { (*QUEUE_MEMORY.0.get()).bytes.as_mut_ptr() }
}

unsafe fn queue_desc_ptr() -> *mut VirtqDesc {
    queue_memory_base() as *mut VirtqDesc
}

unsafe fn queue_avail_ptr() -> *mut VirtqAvail {
    // SAFETY: pointer arithmetic stays within the statically allocated queue region.
    unsafe { queue_memory_base().add(DESC_BYTES) as *mut VirtqAvail }
}

unsafe fn queue_used_ptr() -> *mut VirtqUsed {
    // SAFETY: pointer arithmetic stays within the statically allocated queue region.
    unsafe { queue_memory_base().add(USED_OFFSET) as *mut VirtqUsed }
}

fn request_header_ptr() -> *mut VirtioBlkReqHeader {
    // SAFETY: caller ensures serialized access to request memory.
    unsafe { addr_of_mut!((*REQUEST_MEMORY.0.get()).header) }
}

fn request_data_ptr() -> *mut u8 {
    // SAFETY: caller ensures serialized access to request memory.
    unsafe { (*REQUEST_MEMORY.0.get()).data.as_mut_ptr() }
}

fn request_status_ptr() -> *mut u8 {
    // SAFETY: caller ensures serialized access to request memory.
    unsafe { addr_of_mut!((*REQUEST_MEMORY.0.get()).status) }
}

fn find_virtio_blk_pci() -> Option<PciLocation> {
    for bus in 0u16..=255u16 {
        for device in 0u16..32u16 {
            for function in 0u16..8u16 {
                let vendor = pci_read_u16(bus as u8, device as u8, function as u8, 0x00);
                if vendor == 0xFFFF {
                    if function == 0 {
                        break;
                    }
                    continue;
                }
                let device_id = pci_read_u16(bus as u8, device as u8, function as u8, 0x02);
                if vendor != VIRTIO_VENDOR_ID {
                    continue;
                }
                if device_id != VIRTIO_BLK_TRANSITIONAL_ID && device_id != VIRTIO_BLK_MODERN_ID {
                    continue;
                }

                let bar0 = pci_read_u32(bus as u8, device as u8, function as u8, 0x10);
                if (bar0 & 0x1) == 0 {
                    continue;
                }

                let io_base = (bar0 & !0x3) as u16;
                let command = pci_read_u16(bus as u8, device as u8, function as u8, 0x04);
                let command = command | 0x1 | 0x4;
                pci_write_u16(bus as u8, device as u8, function as u8, 0x04, command);

                return Some(PciLocation {
                    bus: bus as u8,
                    device: device as u8,
                    function: function as u8,
                    device_id,
                    io_base,
                });
            }
        }
    }
    None
}

fn pci_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

fn pci_read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address = pci_address(bus, device, function, offset);
    // SAFETY: x86 PCI config mechanism #1 uses 0xCF8/0xCFC I/O ports.
    unsafe {
        port::outl(PCI_CONFIG_ADDR, address);
        port::inl(PCI_CONFIG_DATA)
    }
}

fn pci_write_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let address = pci_address(bus, device, function, offset);
    // SAFETY: x86 PCI config mechanism #1 uses 0xCF8/0xCFC I/O ports.
    unsafe {
        port::outl(PCI_CONFIG_ADDR, address);
        port::outl(PCI_CONFIG_DATA, value);
    }
}

fn pci_read_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let value = pci_read_u32(bus, device, function, offset);
    let shift = ((offset & 0x2) * 8) as u32;
    ((value >> shift) & 0xFFFF) as u16
}

fn pci_write_u16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    let aligned_offset = offset & !0x2;
    let mut dword = pci_read_u32(bus, device, function, aligned_offset);
    let shift = ((offset & 0x2) * 8) as u32;
    dword &= !(0xFFFFu32 << shift);
    dword |= (value as u32) << shift;
    pci_write_u32(bus, device, function, aligned_offset, dword);
}

struct SpinLock {
    locked: AtomicBool,
}

impl SpinLock {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> SpinLockGuard<'_> {
        while self.locked.swap(true, Ordering::Acquire) {
            spin_loop();
        }
        SpinLockGuard { lock: self }
    }
}

struct SpinLockGuard<'a> {
    lock: &'a SpinLock,
}

impl Drop for SpinLockGuard<'_> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}
