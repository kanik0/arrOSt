// kernel/src/audio/virtio_sound.rs: modern virtio-sound playback backend (PCM TX queue).
use crate::arch::x86_64::port;
use crate::mem;
use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::mem::size_of;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_SOUND_MODERN_ID: u16 = 0x1059;
const VIRTIO_SOUND_TRANSITIONAL_ID: u16 = 0x1018;

const PCI_CONFIG_ADDR: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;
const PCI_STATUS_CAP_LIST: u16 = 1 << 4;
const PCI_CAP_ID_VENDOR_SPECIFIC: u8 = 0x09;

const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

const VIRTIO_STATUS_ACK: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FEATURES_OK: u8 = 8;
const VIRTIO_STATUS_FAILED: u8 = 128;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

const CTRL_QUEUE_INDEX: u16 = 0;
const TX_QUEUE_INDEX: u16 = 2;
const CTRL_QUEUE_SIZE_U16: u16 = 8;
const TX_QUEUE_SIZE_U16: u16 = 32;
const CTRL_QUEUE_SIZE: usize = CTRL_QUEUE_SIZE_U16 as usize;
const TX_QUEUE_SIZE: usize = TX_QUEUE_SIZE_U16 as usize;
const TX_SLOT_COUNT: usize = 10;
const TX_DESC_PER_SLOT: usize = 3;

const MAX_CONTROL_SPINS: usize = 2_000_000;
const MAX_TX_CHUNK_FRAMES: usize = 1024;
const MAX_RESAMPLE_FRAMES: usize = 2048;
const MAX_STREAM_CHANNELS: usize = 2;
const MAX_RESAMPLE_SAMPLES: usize = MAX_RESAMPLE_FRAMES * MAX_STREAM_CHANNELS;
const TX_PACKET_FRAMES: usize = 1024;
const TX_PACKET_SAMPLES: usize = TX_PACKET_FRAMES * 2;
const PCM_BUFFER_PERIODS: u32 = 8;
const PCM_FIFO_FRAMES: usize = TX_PACKET_FRAMES * 24;
const PCM_FIFO_SAMPLES: usize = PCM_FIFO_FRAMES * MAX_STREAM_CHANNELS;
const PCM_FIFO_TARGET_FRAMES: u32 = TX_PACKET_FRAMES as u32 * 6;
const PCM_FIFO_HIGH_WATER_FRAMES: u32 = TX_PACKET_FRAMES as u32 * 10;

const VIRTIO_SND_R_PCM_INFO: u32 = 0x0100;
const VIRTIO_SND_R_PCM_SET_PARAMS: u32 = 0x0101;
const VIRTIO_SND_R_PCM_PREPARE: u32 = 0x0102;
const VIRTIO_SND_R_PCM_START: u32 = 0x0104;
const VIRTIO_SND_R_PCM_STOP: u32 = 0x0105;

const VIRTIO_SND_S_OK: u32 = 0x8000;

const VIRTIO_SND_D_OUTPUT: u8 = 0;

const VIRTIO_SND_PCM_FMT_S16: u8 = 5;
const RATE_ENUM_11025: u8 = 2;
const RATE_ENUM_22050: u8 = 4;
const RATE_ENUM_44100: u8 = 6;
const RATE_ENUM_48000: u8 = 7;

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

impl VirtqDesc {
    const EMPTY: Self = Self {
        addr: 0,
        len: 0,
        flags: 0,
        next: 0,
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqUsedElem {
    id: u32,
    len: u32,
}

impl VirtqUsedElem {
    const EMPTY: Self = Self { id: 0, len: 0 };
}

#[repr(C)]
struct VirtqAvail<const N: usize> {
    flags: u16,
    idx: u16,
    ring: [u16; N],
    used_event: u16,
}

impl<const N: usize> VirtqAvail<N> {
    const fn new() -> Self {
        Self {
            flags: 0,
            idx: 0,
            ring: [0; N],
            used_event: 0,
        }
    }
}

#[repr(C)]
struct VirtqUsed<const N: usize> {
    flags: u16,
    idx: u16,
    ring: [VirtqUsedElem; N],
    avail_event: u16,
}

impl<const N: usize> VirtqUsed<N> {
    const fn new() -> Self {
        Self {
            flags: 0,
            idx: 0,
            ring: [VirtqUsedElem::EMPTY; N],
            avail_event: 0,
        }
    }
}

#[repr(C, align(4096))]
struct QueueMemory<const N: usize> {
    desc: [VirtqDesc; N],
    avail: VirtqAvail<N>,
    used: VirtqUsed<N>,
}

impl<const N: usize> QueueMemory<N> {
    const fn new() -> Self {
        Self {
            desc: [VirtqDesc::EMPTY; N],
            avail: VirtqAvail::new(),
            used: VirtqUsed::new(),
        }
    }

    fn reset(&mut self) {
        self.desc.fill(VirtqDesc::EMPTY);
        self.avail.flags = 0;
        self.avail.idx = 0;
        self.avail.ring.fill(0);
        self.avail.used_event = 0;
        self.used.flags = 0;
        self.used.idx = 0;
        self.used.ring.fill(VirtqUsedElem::EMPTY);
        self.used.avail_event = 0;
    }
}

struct QueueMemoryCell<const N: usize>(UnsafeCell<QueueMemory<N>>);

// SAFETY: access is serialized by the single-threaded kernel main loop.
unsafe impl<const N: usize> Sync for QueueMemoryCell<N> {}

static CTRL_QUEUE_MEMORY: QueueMemoryCell<CTRL_QUEUE_SIZE> =
    QueueMemoryCell(UnsafeCell::new(QueueMemory::new()));
static TX_QUEUE_MEMORY: QueueMemoryCell<TX_QUEUE_SIZE> =
    QueueMemoryCell(UnsafeCell::new(QueueMemory::new()));

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioPciCommonCfg {
    device_feature_select: u32,
    device_feature: u32,
    guest_feature_select: u32,
    guest_feature: u32,
    msix_config: u16,
    num_queues: u16,
    device_status: u8,
    config_generation: u8,
    queue_select: u16,
    queue_size: u16,
    queue_msix_vector: u16,
    queue_enable: u16,
    queue_notify_off: u16,
    queue_desc: u64,
    queue_avail: u64,
    queue_used: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioSndConfig {
    jacks: u32,
    streams: u32,
    chmaps: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioSndHdr {
    code: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioSndQueryInfo {
    hdr: VirtioSndHdr,
    start_id: u32,
    count: u32,
    size: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioSndPcmInfo {
    hda_fn_nid: u32,
    features: u32,
    formats: u64,
    rates: u64,
    direction: u8,
    channels_min: u8,
    channels_max: u8,
    _padding: [u8; 5],
}

impl VirtioSndPcmInfo {
    const EMPTY: Self = Self {
        hda_fn_nid: 0,
        features: 0,
        formats: 0,
        rates: 0,
        direction: 0xff,
        channels_min: 0,
        channels_max: 0,
        _padding: [0; 5],
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioSndPcmSetParams {
    hdr: VirtioSndHdr,
    stream_id: u32,
    buffer_bytes: u32,
    period_bytes: u32,
    features: u32,
    channels: u8,
    format: u8,
    rate: u8,
    _padding: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioSndPcmHdr {
    hdr: VirtioSndHdr,
    stream_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioSndPcmXfer {
    stream_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioSndPcmStatus {
    status: u32,
    latency_bytes: u32,
}

#[repr(C, align(16))]
struct TxPacket {
    xfer: VirtioSndPcmXfer,
    pcm: [i16; TX_PACKET_SAMPLES],
    status: VirtioSndPcmStatus,
}

impl TxPacket {
    const fn new() -> Self {
        Self {
            xfer: VirtioSndPcmXfer { stream_id: 0 },
            pcm: [0; TX_PACKET_SAMPLES],
            status: VirtioSndPcmStatus {
                status: 0,
                latency_bytes: 0,
            },
        }
    }
}

struct TxPacketsCell(UnsafeCell<[TxPacket; TX_SLOT_COUNT]>);

// SAFETY: access is serialized by the single-threaded kernel main loop.
unsafe impl Sync for TxPacketsCell {}

static TX_PACKETS: TxPacketsCell = TxPacketsCell(UnsafeCell::new([
    TxPacket::new(),
    TxPacket::new(),
    TxPacket::new(),
    TxPacket::new(),
    TxPacket::new(),
    TxPacket::new(),
    TxPacket::new(),
    TxPacket::new(),
    TxPacket::new(),
    TxPacket::new(),
]));

#[derive(Clone, Copy)]
struct CapRegion {
    bar: u8,
    offset: u32,
    length: u32,
}

#[derive(Clone, Copy)]
struct PciLocation {
    bus: u8,
    device: u8,
    function: u8,
    device_id: u16,
}

#[derive(Clone, Copy)]
struct PciCaps {
    common: CapRegion,
    notify: CapRegion,
    notify_multiplier: u32,
    isr: Option<CapRegion>,
    device: CapRegion,
}

#[derive(Clone, Copy)]
struct QueueHandle {
    size: u16,
    notify_off: u16,
    last_used_idx: u16,
}

#[derive(Clone, Copy)]
pub struct VirtioSoundInitReport {
    pub ready: bool,
    pub stream_id: u32,
    pub sample_rate_hz: u32,
    pub channels: u8,
    pub device_id: u16,
    pub reason: &'static str,
}

#[derive(Clone, Copy)]
pub struct VirtioSoundStatus {
    pub ready: bool,
    pub stream_id: u32,
    pub sample_rate_hz: u32,
    pub channels: u8,
    pub pending_packets: u16,
    pub buffered_frames: u32,
    pub submitted_packets: u64,
    pub completed_packets: u64,
    pub dropped_packets: u64,
    pub completed_frames: u64,
    pub dropped_frames: u64,
    pub last_ctrl_status: u32,
}

struct DriverCell(UnsafeCell<DriverState>);

// SAFETY: this module is used from the single-threaded kernel runtime loop.
unsafe impl Sync for DriverCell {}

static DRIVER_STATE: DriverCell = DriverCell(UnsafeCell::new(DriverState::new()));

struct DriverState {
    initialized: bool,
    ready: bool,
    reason: &'static str,
    pci_device_id: u16,
    common_cfg: *mut VirtioPciCommonCfg,
    notify_base: *mut u8,
    notify_multiplier: u32,
    device_cfg: *const VirtioSndConfig,
    ctrl_queue: QueueHandle,
    tx_queue: QueueHandle,
    stream_id: u32,
    stream_rate_hz: u32,
    stream_rate_enum: u8,
    channels: u8,
    started: bool,
    resample_phase_fp: u64,
    tx_slot_busy: [bool; TX_SLOT_COUNT],
    tx_slot_frames: [u16; TX_SLOT_COUNT],
    resample_tmp: [i16; MAX_RESAMPLE_SAMPLES],
    pcm_fifo: [i16; PCM_FIFO_SAMPLES],
    pcm_fifo_read: usize,
    pcm_fifo_write: usize,
    pcm_fifo_samples: usize,
    pending_hw_frames: u32,
    ctrl_status: VirtioSndHdr,
    pcm_infos: [VirtioSndPcmInfo; TX_SLOT_COUNT],
    pending_packets: u16,
    submitted_packets: u64,
    completed_packets: u64,
    dropped_packets: u64,
    completed_frames: u64,
    dropped_frames: u64,
    last_ctrl_status: u32,
}

impl DriverState {
    const fn new() -> Self {
        Self {
            initialized: false,
            ready: false,
            reason: "not_initialized",
            pci_device_id: 0,
            common_cfg: core::ptr::null_mut(),
            notify_base: core::ptr::null_mut(),
            notify_multiplier: 0,
            device_cfg: core::ptr::null(),
            ctrl_queue: QueueHandle {
                size: 0,
                notify_off: 0,
                last_used_idx: 0,
            },
            tx_queue: QueueHandle {
                size: 0,
                notify_off: 0,
                last_used_idx: 0,
            },
            stream_id: 0,
            stream_rate_hz: 0,
            stream_rate_enum: 0,
            channels: 0,
            started: false,
            resample_phase_fp: 0,
            tx_slot_busy: [false; TX_SLOT_COUNT],
            tx_slot_frames: [0; TX_SLOT_COUNT],
            resample_tmp: [0; MAX_RESAMPLE_SAMPLES],
            pcm_fifo: [0; PCM_FIFO_SAMPLES],
            pcm_fifo_read: 0,
            pcm_fifo_write: 0,
            pcm_fifo_samples: 0,
            pending_hw_frames: 0,
            ctrl_status: VirtioSndHdr { code: 0 },
            pcm_infos: [VirtioSndPcmInfo::EMPTY; TX_SLOT_COUNT],
            pending_packets: 0,
            submitted_packets: 0,
            completed_packets: 0,
            dropped_packets: 0,
            completed_frames: 0,
            dropped_frames: 0,
            last_ctrl_status: 0,
        }
    }

    fn report(&self) -> VirtioSoundInitReport {
        VirtioSoundInitReport {
            ready: self.ready,
            stream_id: self.stream_id,
            sample_rate_hz: self.stream_rate_hz,
            channels: self.channels,
            device_id: self.pci_device_id,
            reason: self.reason,
        }
    }

    fn status(&self) -> VirtioSoundStatus {
        VirtioSoundStatus {
            ready: self.ready,
            stream_id: self.stream_id,
            sample_rate_hz: self.stream_rate_hz,
            channels: self.channels,
            pending_packets: self.pending_packets,
            buffered_frames: self.total_buffered_frames(),
            submitted_packets: self.submitted_packets,
            completed_packets: self.completed_packets,
            dropped_packets: self.dropped_packets,
            completed_frames: self.completed_frames,
            dropped_frames: self.dropped_frames,
            last_ctrl_status: self.last_ctrl_status,
        }
    }

    fn init_once(&mut self) -> VirtioSoundInitReport {
        if self.initialized {
            return self.report();
        }
        self.initialized = true;

        let result = self.try_init();
        if let Err(reason) = result {
            self.ready = false;
            self.reason = reason;
            self.fail_device();
        }

        self.report()
    }

    fn reset_runtime_metrics(&mut self) {
        self.pending_packets = 0;
        self.submitted_packets = 0;
        self.completed_packets = 0;
        self.dropped_packets = 0;
        self.completed_frames = 0;
        self.dropped_frames = 0;
        self.resample_phase_fp = 0;
        self.pending_hw_frames = 0;
        self.pcm_fifo_read = 0;
        self.pcm_fifo_write = 0;
        self.pcm_fifo_samples = 0;
        self.pcm_fifo.fill(0);
    }

    fn try_init(&mut self) -> Result<(), &'static str> {
        let (pci, caps) = find_virtio_sound_pci().ok_or("virtio_snd_not_found")?;
        self.pci_device_id = pci.device_id;

        let common_cfg_ptr = map_cap_region(&pci, caps.common).ok_or("virtio_snd_common_map")?;
        let notify_ptr = map_cap_region(&pci, caps.notify).ok_or("virtio_snd_notify_map")?;
        let device_cfg_ptr = map_cap_region(&pci, caps.device).ok_or("virtio_snd_device_map")?;
        let _isr_ptr = caps.isr.and_then(|region| map_cap_region(&pci, region));

        self.common_cfg = common_cfg_ptr as *mut VirtioPciCommonCfg;
        self.notify_base = notify_ptr;
        self.notify_multiplier = caps.notify_multiplier.max(2);
        self.device_cfg = device_cfg_ptr as *const VirtioSndConfig;

        // SAFETY: pointers come from validated PCI capabilities and remain stable after init.
        unsafe {
            write_volatile(addr_of_mut!((*self.common_cfg).device_status), 0);
            write_volatile(
                addr_of_mut!((*self.common_cfg).device_status),
                VIRTIO_STATUS_ACK,
            );
            write_volatile(
                addr_of_mut!((*self.common_cfg).device_status),
                VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER,
            );
            write_volatile(addr_of_mut!((*self.common_cfg).device_feature_select), 0);
            let _ = read_volatile(addr_of!((*self.common_cfg).device_feature));
            write_volatile(addr_of_mut!((*self.common_cfg).guest_feature_select), 0);
            write_volatile(addr_of_mut!((*self.common_cfg).guest_feature), 0);
            write_volatile(addr_of_mut!((*self.common_cfg).guest_feature_select), 1);
            write_volatile(addr_of_mut!((*self.common_cfg).guest_feature), 0);
            write_volatile(
                addr_of_mut!((*self.common_cfg).device_status),
                VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
            );
            let status = read_volatile(addr_of!((*self.common_cfg).device_status));
            if (status & VIRTIO_STATUS_FEATURES_OK) == 0 {
                return Err("virtio_snd_features_rejected");
            }
        }

        // SAFETY: queue memory is private to this driver and initialized once.
        unsafe {
            (*CTRL_QUEUE_MEMORY.0.get()).reset();
            (*TX_QUEUE_MEMORY.0.get()).reset();
        }

        self.ctrl_queue =
            self.setup_queue::<CTRL_QUEUE_SIZE>(CTRL_QUEUE_INDEX, CTRL_QUEUE_SIZE_U16, unsafe {
                &mut *CTRL_QUEUE_MEMORY.0.get()
            })?;
        self.tx_queue =
            self.setup_queue::<TX_QUEUE_SIZE>(TX_QUEUE_INDEX, TX_QUEUE_SIZE_U16, unsafe {
                &mut *TX_QUEUE_MEMORY.0.get()
            })?;

        let cfg = self.read_device_cfg();
        if cfg.streams == 0 {
            return Err("virtio_snd_no_streams");
        }

        let stream_count = cfg.streams.min(self.pcm_infos.len() as u32);
        self.query_pcm_info(stream_count)
            .map_err(|reason| self.ctrl_error_reason(reason))?;
        self.select_output_stream(stream_count)
            .ok_or("virtio_snd_no_output_stream")?;

        self.send_set_params()
            .map_err(|reason| self.ctrl_error_reason(reason))?;
        self.send_pcm_cmd(VIRTIO_SND_R_PCM_PREPARE)
            .map_err(|reason| self.ctrl_error_reason(reason))?;
        self.send_pcm_cmd(VIRTIO_SND_R_PCM_START)
            .map_err(|reason| self.ctrl_error_reason(reason))?;
        self.started = true;

        self.reset_runtime_metrics();
        self.reason = "ok";
        self.ready = true;

        // SAFETY: same invariants as above for common cfg access.
        unsafe {
            write_volatile(
                addr_of_mut!((*self.common_cfg).device_status),
                VIRTIO_STATUS_ACK
                    | VIRTIO_STATUS_DRIVER
                    | VIRTIO_STATUS_FEATURES_OK
                    | VIRTIO_STATUS_DRIVER_OK,
            );
        }

        Ok(())
    }

    fn fail_device(&mut self) {
        if self.common_cfg.is_null() {
            return;
        }
        // SAFETY: if common_cfg is non-null it points to valid mapped common cfg.
        unsafe {
            let status = read_volatile(addr_of!((*self.common_cfg).device_status));
            write_volatile(
                addr_of_mut!((*self.common_cfg).device_status),
                status | VIRTIO_STATUS_FAILED,
            );
        }
    }

    fn read_device_cfg(&self) -> VirtioSndConfig {
        // SAFETY: device_cfg points to virtio-snd config region discovered via PCI cap.
        unsafe { read_volatile(self.device_cfg) }
    }

    fn setup_queue<const N: usize>(
        &mut self,
        queue_index: u16,
        desired_size: u16,
        memory: &mut QueueMemory<N>,
    ) -> Result<QueueHandle, &'static str> {
        // SAFETY: common_cfg pointer was validated during init and queue access is serialized.
        unsafe {
            write_volatile(addr_of_mut!((*self.common_cfg).queue_select), queue_index);
            let max_size = read_volatile(addr_of!((*self.common_cfg).queue_size));
            if max_size == 0 {
                return Err("virtio_snd_queue_unavailable");
            }

            let size = floor_pow2(max_size.min(desired_size));
            if size == 0 {
                return Err("virtio_snd_queue_size_zero");
            }
            if usize::from(size) > N {
                return Err("virtio_snd_queue_size_too_big");
            }

            let desc_ptr = addr_of_mut!(memory.desc) as *mut VirtqDesc;
            let avail_ptr = addr_of_mut!(memory.avail);
            let used_ptr = addr_of_mut!(memory.used);

            let desc_phys =
                mem::virt_to_phys(desc_ptr as usize).ok_or("virtio_snd_desc_phys_missing")?;
            let avail_phys =
                mem::virt_to_phys(avail_ptr as usize).ok_or("virtio_snd_avail_phys_missing")?;
            let used_phys =
                mem::virt_to_phys(used_ptr as usize).ok_or("virtio_snd_used_phys_missing")?;

            write_volatile(addr_of_mut!((*self.common_cfg).queue_size), size);
            write_volatile(addr_of_mut!((*self.common_cfg).queue_desc), desc_phys);
            write_volatile(addr_of_mut!((*self.common_cfg).queue_avail), avail_phys);
            write_volatile(addr_of_mut!((*self.common_cfg).queue_used), used_phys);
            write_volatile(addr_of_mut!((*self.common_cfg).queue_enable), 1);

            let notify_off = read_volatile(addr_of!((*self.common_cfg).queue_notify_off));
            Ok(QueueHandle {
                size,
                notify_off,
                last_used_idx: 0,
            })
        }
    }

    fn notify_queue(&self, queue_index: u16, notify_off: u16) {
        let offset = usize::from(notify_off).saturating_mul(self.notify_multiplier as usize);
        // SAFETY: notify_base maps the virtio notify region and offset comes from device queue cfg.
        let notify_ptr = unsafe { self.notify_base.add(offset) as *mut u16 };
        // SAFETY: writing queue index to queue notify register is required by virtio-pci transport.
        unsafe {
            write_volatile(notify_ptr, queue_index);
        }
    }

    fn send_ctrl(
        &mut self,
        req_ptr: *const u8,
        req_len: usize,
        resp_ptr: *mut u8,
        resp_len: usize,
    ) -> Result<u32, &'static str> {
        if req_len == 0 {
            return Err("virtio_snd_ctrl_req_empty");
        }

        // SAFETY: queue memory is private to this driver and serialized by the main loop.
        let queue = unsafe { &mut *CTRL_QUEUE_MEMORY.0.get() };
        let queue_size = usize::from(self.ctrl_queue.size);
        if queue_size < 3 {
            return Err("virtio_snd_ctrl_queue_small");
        }

        let req_phys = mem::virt_to_phys(req_ptr as usize).ok_or("virtio_snd_ctrl_req_phys")?;
        let status_phys = mem::virt_to_phys(addr_of!(self.ctrl_status) as usize)
            .ok_or("virtio_snd_ctrl_status_phys")?;
        let status_desc = if resp_len > 0 {
            let resp_phys =
                mem::virt_to_phys(resp_ptr as usize).ok_or("virtio_snd_ctrl_resp_phys")?;
            queue.desc[0] = VirtqDesc {
                addr: req_phys,
                len: req_len as u32,
                flags: VIRTQ_DESC_F_NEXT,
                next: 1,
            };
            queue.desc[1] = VirtqDesc {
                addr: resp_phys,
                len: resp_len as u32,
                flags: VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
                next: 2,
            };
            2usize
        } else {
            queue.desc[0] = VirtqDesc {
                addr: req_phys,
                len: req_len as u32,
                flags: VIRTQ_DESC_F_NEXT,
                next: 1,
            };
            1usize
        };

        queue.desc[status_desc] = VirtqDesc {
            addr: status_phys,
            len: size_of::<VirtioSndHdr>() as u32,
            flags: VIRTQ_DESC_F_WRITE,
            next: 0,
        };

        self.ctrl_status.code = 0;
        let avail_slot = (queue.avail.idx % self.ctrl_queue.size) as usize;
        queue.avail.ring[avail_slot] = 0;
        fence(Ordering::Release);
        queue.avail.idx = queue.avail.idx.wrapping_add(1);
        self.notify_queue(CTRL_QUEUE_INDEX, self.ctrl_queue.notify_off);

        let target_used = self.ctrl_queue.last_used_idx.wrapping_add(1);
        for _ in 0..MAX_CONTROL_SPINS {
            // SAFETY: `used.idx` belongs to this queue memory.
            let used_idx = unsafe { read_volatile(addr_of!(queue.used.idx)) };
            if used_idx == target_used {
                self.ctrl_queue.last_used_idx = used_idx;
                let status = self.ctrl_status.code;
                self.last_ctrl_status = status;
                if status != VIRTIO_SND_S_OK && status != 0 {
                    return Err("virtio_snd_ctrl_status");
                }
                return Ok(status);
            }
            spin_loop();
        }

        Err("virtio_snd_ctrl_timeout")
    }

    fn ctrl_error_reason(&self, reason: &'static str) -> &'static str {
        if reason != "virtio_snd_ctrl_status" {
            return reason;
        }
        match self.last_ctrl_status {
            0 => "virtio_snd_ctrl_status_0",
            0x8001 => "virtio_snd_ctrl_bad_msg",
            0x8002 => "virtio_snd_ctrl_not_supp",
            0x8003 => "virtio_snd_ctrl_io_err",
            _ => "virtio_snd_ctrl_status_unknown",
        }
    }

    fn query_pcm_info(&mut self, count: u32) -> Result<(), &'static str> {
        self.pcm_infos.fill(VirtioSndPcmInfo::EMPTY);
        let query = VirtioSndQueryInfo {
            hdr: VirtioSndHdr {
                code: VIRTIO_SND_R_PCM_INFO,
            },
            start_id: 0,
            count,
            size: size_of::<VirtioSndPcmInfo>() as u32,
        };
        let resp_len = (count as usize)
            .min(self.pcm_infos.len())
            .saturating_mul(size_of::<VirtioSndPcmInfo>());
        let info_ptr = core::ptr::addr_of_mut!(self.pcm_infos) as *mut VirtioSndPcmInfo;
        self.send_ctrl(
            addr_of!(query) as *const u8,
            size_of::<VirtioSndQueryInfo>(),
            info_ptr as *mut u8,
            resp_len,
        )?;
        Ok(())
    }

    fn select_output_stream(&mut self, count: u32) -> Option<()> {
        let available = (count as usize).min(self.pcm_infos.len());
        let mut selected: Option<(u32, u8, u32)> = None;

        for stream_id in 0..available {
            let info = self.pcm_infos[stream_id];
            if info.direction != VIRTIO_SND_D_OUTPUT {
                continue;
            }
            if (info.formats & (1u64 << VIRTIO_SND_PCM_FMT_S16)) == 0 {
                continue;
            }
            let Some((rate_enum, rate_hz)) = choose_rate(info.rates) else {
                continue;
            };
            let channels = if info.channels_min <= 2 && info.channels_max >= 2 {
                2
            } else if info.channels_min <= 1 && info.channels_max >= 1 {
                1
            } else {
                continue;
            };
            selected = Some((stream_id as u32, rate_enum, rate_hz));
            self.channels = channels;
            break;
        }

        if selected.is_none() && available > 0 {
            selected = Some((0, RATE_ENUM_44100, 44_100));
            self.channels = 2;
        }

        let (stream_id, rate_enum, rate_hz) = selected?;
        self.stream_id = stream_id;
        self.stream_rate_enum = rate_enum;
        self.stream_rate_hz = rate_hz;
        Some(())
    }

    fn send_set_params(&mut self) -> Result<(), &'static str> {
        let frame_bytes = usize::from(self.channels).saturating_mul(size_of::<i16>());
        let period_bytes = TX_PACKET_FRAMES.saturating_mul(frame_bytes) as u32;
        let buffer_bytes = period_bytes.saturating_mul(PCM_BUFFER_PERIODS);
        let params = VirtioSndPcmSetParams {
            hdr: VirtioSndHdr {
                code: VIRTIO_SND_R_PCM_SET_PARAMS,
            },
            stream_id: self.stream_id,
            buffer_bytes,
            period_bytes,
            features: 0,
            channels: self.channels,
            format: VIRTIO_SND_PCM_FMT_S16,
            rate: self.stream_rate_enum,
            _padding: 0,
        };
        self.send_ctrl(
            addr_of!(params) as *const u8,
            size_of::<VirtioSndPcmSetParams>(),
            core::ptr::null_mut(),
            0,
        )?;
        Ok(())
    }

    fn send_pcm_cmd(&mut self, code: u32) -> Result<(), &'static str> {
        let cmd = VirtioSndPcmHdr {
            hdr: VirtioSndHdr { code },
            stream_id: self.stream_id,
        };
        self.send_ctrl(
            addr_of!(cmd) as *const u8,
            size_of::<VirtioSndPcmHdr>(),
            core::ptr::null_mut(),
            0,
        )?;
        Ok(())
    }

    fn poll_tx_used(&mut self) {
        if !self.ready {
            return;
        }
        // SAFETY: queue memory is private to this driver and polling is serialized.
        let queue = unsafe { &mut *TX_QUEUE_MEMORY.0.get() };
        loop {
            // SAFETY: `used.idx` belongs to TX queue memory.
            let used_idx = unsafe { read_volatile(addr_of!(queue.used.idx)) };
            if self.tx_queue.last_used_idx == used_idx {
                break;
            }

            let used_slot = (self.tx_queue.last_used_idx % self.tx_queue.size) as usize;
            let head = queue.used.ring[used_slot].id as usize;
            let slot = head / TX_DESC_PER_SLOT;
            if slot < TX_SLOT_COUNT && self.tx_slot_busy[slot] {
                self.tx_slot_busy[slot] = false;
                self.pending_packets = self.pending_packets.saturating_sub(1);
                self.completed_packets = self.completed_packets.saturating_add(1);
                let frame_count = u32::from(self.tx_slot_frames[slot]);
                self.pending_hw_frames = self.pending_hw_frames.saturating_sub(frame_count);
                self.completed_frames =
                    self.completed_frames.saturating_add(u64::from(frame_count));
                self.tx_slot_frames[slot] = 0;
                // SAFETY: TX packet slot belongs to this driver and is only accessed while serialized.
                let packet = unsafe { &(*TX_PACKETS.0.get())[slot] };
                if packet.status.status != VIRTIO_SND_S_OK && packet.status.status != 0 {
                    self.dropped_packets = self.dropped_packets.saturating_add(1);
                    self.last_ctrl_status = packet.status.status;
                }
            }

            self.tx_queue.last_used_idx = self.tx_queue.last_used_idx.wrapping_add(1);
        }
    }

    fn set_started(&mut self, enabled: bool) {
        if !self.ready {
            return;
        }
        if enabled == self.started {
            return;
        }
        let result = if enabled {
            self.send_pcm_cmd(VIRTIO_SND_R_PCM_START)
        } else {
            self.send_pcm_cmd(VIRTIO_SND_R_PCM_STOP)
        };
        if result.is_ok() {
            self.started = enabled;
            if !enabled {
                self.pcm_fifo_read = 0;
                self.pcm_fifo_write = 0;
                self.pcm_fifo_samples = 0;
                self.resample_phase_fp = 0;
            } else {
                self.pump_fifo_to_tx();
            }
        }
    }

    fn submit_pcm_i16(&mut self, samples: &[i16], src_rate: u32, src_channels: u8) -> usize {
        if !self.ready || !self.started || samples.is_empty() || src_rate == 0 {
            return 0;
        }
        let input_channels = src_channels.clamp(1, 2) as usize;
        let src_frames = samples.len() / input_channels;
        if src_frames == 0 {
            return 0;
        }
        self.pump_fifo_to_tx();

        let mut consumed_frames = 0usize;
        let src_step_fp = ((u64::from(src_rate)) << 32) / u64::from(self.stream_rate_hz.max(1));
        let mut phase = self.resample_phase_fp;
        let output_channels = usize::from(self.channels.clamp(1, 2));
        let source_samples = &samples[..src_frames.saturating_mul(input_channels)];

        for chunk in source_samples.chunks(MAX_TX_CHUNK_FRAMES.saturating_mul(input_channels)) {
            if chunk.is_empty() {
                continue;
            }
            let chunk_frames = chunk.len() / input_channels;
            if chunk_frames == 0 {
                continue;
            }

            let mut produced_frames = 0usize;
            while produced_frames < MAX_RESAMPLE_FRAMES {
                let src_idx = (phase >> 32) as usize;
                if src_idx >= chunk_frames {
                    break;
                }

                let next_idx = (src_idx + 1).min(chunk_frames.saturating_sub(1));
                let src_base = src_idx.saturating_mul(input_channels);
                let next_base = next_idx.saturating_mul(input_channels);
                let frac = ((phase & 0xFFFF_FFFF) >> 16) as u32;

                let src_l = lerp_i16(chunk[src_base], chunk[next_base], frac);
                let src_r = if input_channels > 1 {
                    lerp_i16(chunk[src_base + 1], chunk[next_base + 1], frac)
                } else {
                    src_l
                };
                let out_base = produced_frames.saturating_mul(output_channels);
                self.resample_tmp[out_base] = src_l;
                if output_channels > 1 {
                    self.resample_tmp[out_base + 1] = src_r;
                }

                produced_frames += 1;
                phase = phase.saturating_add(src_step_fp.max(1));
            }
            let chunk_limit = (chunk_frames as u64) << 32;
            if phase >= chunk_limit {
                phase = phase.saturating_sub(chunk_limit);
            }

            consumed_frames = consumed_frames.saturating_add(chunk_frames);
            let produced_samples = produced_frames.saturating_mul(output_channels);
            if produced_samples > 0 {
                let mut local = [0i16; MAX_RESAMPLE_SAMPLES];
                local[..produced_samples].copy_from_slice(&self.resample_tmp[..produced_samples]);
                self.push_fifo_samples(&local[..produced_samples], output_channels);
                self.trim_fifo_if_needed(output_channels);
                self.pump_fifo_to_tx();
            }
        }

        self.resample_phase_fp = phase;
        consumed_frames.saturating_mul(input_channels)
    }

    fn enqueue_tx_packet(
        &mut self,
        interleaved_samples: &[i16],
        frames: usize,
        channels: usize,
    ) -> bool {
        let Some(slot) = self.next_free_slot() else {
            return false;
        };
        if frames == 0 || interleaved_samples.is_empty() {
            return true;
        }

        if channels == 0 || channels > 2 || frames > TX_PACKET_FRAMES {
            return false;
        }

        let stream_channels = usize::from(self.channels.clamp(1, 2));
        let required_samples = frames.saturating_mul(channels);
        if required_samples > interleaved_samples.len() {
            return false;
        }

        let frame_count = frames.min(TX_PACKET_FRAMES);
        let sample_count = frame_count.saturating_mul(stream_channels);
        if sample_count > TX_PACKET_SAMPLES {
            return false;
        }

        // SAFETY: slot is exclusively owned by this function until queued to device.
        let packet = unsafe { &mut (*TX_PACKETS.0.get())[slot] };
        packet.xfer.stream_id = self.stream_id;
        packet.status.status = 0;
        packet.status.latency_bytes = 0;
        if stream_channels == 2 {
            if channels == 2 {
                packet.pcm[..sample_count].copy_from_slice(&interleaved_samples[..sample_count]);
            } else {
                for (idx, sample) in interleaved_samples[..frame_count]
                    .iter()
                    .copied()
                    .enumerate()
                {
                    let out = idx * 2;
                    packet.pcm[out] = sample;
                    packet.pcm[out + 1] = sample;
                }
            }
        } else {
            if channels == 2 {
                for frame in 0..frame_count {
                    let base = frame * 2;
                    let left = i32::from(interleaved_samples[base]);
                    let right = i32::from(interleaved_samples[base + 1]);
                    packet.pcm[frame] = ((left + right) / 2) as i16;
                }
            } else {
                packet.pcm[..frame_count].copy_from_slice(&interleaved_samples[..frame_count]);
            }
        }

        let head = slot * TX_DESC_PER_SLOT;
        let status_idx = head + 2;
        if status_idx >= TX_QUEUE_SIZE {
            return false;
        }

        let xfer_phys = match mem::virt_to_phys(addr_of!(packet.xfer) as usize) {
            Some(value) => value,
            None => return false,
        };
        let pcm_phys = match mem::virt_to_phys(packet.pcm.as_ptr() as usize) {
            Some(value) => value,
            None => return false,
        };
        let status_phys = match mem::virt_to_phys(addr_of!(packet.status) as usize) {
            Some(value) => value,
            None => return false,
        };

        // SAFETY: queue memory is private to this driver and serialized by main loop.
        let queue = unsafe { &mut *TX_QUEUE_MEMORY.0.get() };
        queue.desc[head] = VirtqDesc {
            addr: xfer_phys,
            len: size_of::<VirtioSndPcmXfer>() as u32,
            flags: VIRTQ_DESC_F_NEXT,
            next: (head + 1) as u16,
        };
        queue.desc[head + 1] = VirtqDesc {
            addr: pcm_phys,
            len: (sample_count * size_of::<i16>()) as u32,
            flags: VIRTQ_DESC_F_NEXT,
            next: (head + 2) as u16,
        };
        queue.desc[head + 2] = VirtqDesc {
            addr: status_phys,
            len: size_of::<VirtioSndPcmStatus>() as u32,
            flags: VIRTQ_DESC_F_WRITE,
            next: 0,
        };

        let avail_slot = (queue.avail.idx % self.tx_queue.size) as usize;
        queue.avail.ring[avail_slot] = head as u16;
        fence(Ordering::Release);
        queue.avail.idx = queue.avail.idx.wrapping_add(1);
        self.notify_queue(TX_QUEUE_INDEX, self.tx_queue.notify_off);

        self.tx_slot_busy[slot] = true;
        self.tx_slot_frames[slot] = frame_count as u16;
        self.pending_packets = self.pending_packets.saturating_add(1);
        self.pending_hw_frames = self.pending_hw_frames.saturating_add(frame_count as u32);
        self.submitted_packets = self.submitted_packets.saturating_add(1);
        true
    }

    fn next_free_slot(&self) -> Option<usize> {
        (0..TX_SLOT_COUNT).find(|&slot| !self.tx_slot_busy[slot])
    }

    fn total_buffered_frames(&self) -> u32 {
        let channels = usize::from(self.channels.clamp(1, 2));
        if channels == 0 {
            return self.pending_hw_frames;
        }
        let fifo_frames = (self.pcm_fifo_samples / channels).min(u32::MAX as usize) as u32;
        self.pending_hw_frames.saturating_add(fifo_frames)
    }

    fn fifo_capacity_samples(channels: usize) -> usize {
        PCM_FIFO_FRAMES.saturating_mul(channels.clamp(1, 2))
    }

    fn fifo_drop_oldest_samples(&mut self, drop_samples: usize, channels: usize) {
        if drop_samples == 0 || self.pcm_fifo_samples == 0 {
            return;
        }
        let channels = channels.clamp(1, 2);
        let capacity = Self::fifo_capacity_samples(channels);
        if capacity == 0 {
            return;
        }

        let aligned_drop = drop_samples - (drop_samples % channels);
        if aligned_drop == 0 {
            return;
        }
        let dropped = aligned_drop.min(self.pcm_fifo_samples);
        self.pcm_fifo_read = (self.pcm_fifo_read + dropped) % capacity;
        self.pcm_fifo_samples = self.pcm_fifo_samples.saturating_sub(dropped);
        self.dropped_frames = self
            .dropped_frames
            .saturating_add((dropped / channels) as u64);
    }

    fn push_fifo_samples(&mut self, samples: &[i16], channels: usize) {
        if samples.is_empty() {
            return;
        }
        let channels = channels.clamp(1, 2);
        let capacity = Self::fifo_capacity_samples(channels);
        if capacity == 0 {
            return;
        }

        let mut sample_count = samples.len() - (samples.len() % channels);
        if sample_count == 0 {
            return;
        }

        let mut source_start = 0usize;
        if sample_count > capacity {
            let overflow = sample_count - capacity;
            self.dropped_frames = self
                .dropped_frames
                .saturating_add((overflow / channels) as u64);
            source_start = overflow;
            sample_count = capacity;
        }

        let free = capacity.saturating_sub(self.pcm_fifo_samples);
        if sample_count > free {
            self.fifo_drop_oldest_samples(sample_count - free, channels);
        }

        let mut remaining = sample_count;
        let mut input_offset = source_start;
        while remaining > 0 {
            let contiguous = capacity.saturating_sub(self.pcm_fifo_write);
            let write_now = remaining.min(contiguous);
            let write_end = self.pcm_fifo_write + write_now;
            self.pcm_fifo[self.pcm_fifo_write..write_end]
                .copy_from_slice(&samples[input_offset..input_offset + write_now]);
            self.pcm_fifo_write = (self.pcm_fifo_write + write_now) % capacity;
            self.pcm_fifo_samples = self.pcm_fifo_samples.saturating_add(write_now);
            input_offset += write_now;
            remaining -= write_now;
        }
    }

    fn copy_fifo_prefix(&self, target: &mut [i16], sample_count: usize, channels: usize) -> bool {
        if sample_count == 0 || sample_count > target.len() || sample_count > self.pcm_fifo_samples
        {
            return false;
        }
        let channels = channels.clamp(1, 2);
        let capacity = Self::fifo_capacity_samples(channels);
        if capacity == 0 {
            return false;
        }

        let mut copied = 0usize;
        let mut read = self.pcm_fifo_read;
        while copied < sample_count {
            let contiguous = capacity.saturating_sub(read);
            let copy_now = (sample_count - copied).min(contiguous);
            target[copied..copied + copy_now]
                .copy_from_slice(&self.pcm_fifo[read..read + copy_now]);
            read = (read + copy_now) % capacity;
            copied += copy_now;
        }
        true
    }

    fn consume_fifo_samples(&mut self, sample_count: usize, channels: usize) {
        if sample_count == 0 {
            return;
        }
        let channels = channels.clamp(1, 2);
        let capacity = Self::fifo_capacity_samples(channels);
        if capacity == 0 {
            return;
        }
        let aligned = sample_count - (sample_count % channels);
        let consumed = aligned.min(self.pcm_fifo_samples);
        self.pcm_fifo_read = (self.pcm_fifo_read + consumed) % capacity;
        self.pcm_fifo_samples = self.pcm_fifo_samples.saturating_sub(consumed);
    }

    fn trim_fifo_if_needed(&mut self, channels: usize) {
        let channels = channels.clamp(1, 2);
        let total = self.total_buffered_frames();
        if total <= PCM_FIFO_HIGH_WATER_FRAMES {
            return;
        }
        let fifo_frames = (self.pcm_fifo_samples / channels).min(u32::MAX as usize) as u32;
        if fifo_frames == 0 {
            return;
        }
        let mut drop_frames = total.saturating_sub(PCM_FIFO_TARGET_FRAMES);
        drop_frames = drop_frames.min(fifo_frames);
        let drop_samples = (drop_frames as usize).saturating_mul(channels);
        self.fifo_drop_oldest_samples(drop_samples, channels);
    }

    fn pump_fifo_to_tx(&mut self) {
        if !self.ready || !self.started {
            return;
        }
        self.poll_tx_used();

        let channels = usize::from(self.channels.clamp(1, 2));
        if channels == 0 {
            return;
        }

        let mut local = [0i16; TX_PACKET_SAMPLES];
        loop {
            if self.next_free_slot().is_none() {
                break;
            }
            let available_frames = self.pcm_fifo_samples / channels;
            if available_frames == 0 {
                break;
            }

            let frame_count = available_frames.min(TX_PACKET_FRAMES);
            let sample_count = frame_count.saturating_mul(channels);
            if sample_count == 0 || sample_count > local.len() {
                break;
            }
            if !self.copy_fifo_prefix(&mut local, sample_count, channels) {
                break;
            }
            if !self.enqueue_tx_packet(&local[..sample_count], frame_count, channels) {
                break;
            }
            self.consume_fifo_samples(sample_count, channels);
        }
    }

    fn poll(&mut self) {
        self.pump_fifo_to_tx();
    }
}

pub fn init() -> VirtioSoundInitReport {
    with_state_mut(DriverState::init_once)
}

pub fn status() -> VirtioSoundStatus {
    with_state_mut(|state| state.status())
}

pub fn reset_runtime_metrics() {
    with_state_mut(DriverState::reset_runtime_metrics);
}

pub fn poll() {
    with_state_mut(DriverState::poll);
}

pub fn set_enabled(enabled: bool) {
    with_state_mut(|state| state.set_started(enabled));
}

pub fn submit_pcm_i16(samples: &[i16], sample_rate: u32, channels: u8) -> usize {
    with_state_mut(|state| state.submit_pcm_i16(samples, sample_rate, channels))
}

fn with_state_mut<R>(f: impl FnOnce(&mut DriverState) -> R) -> R {
    // SAFETY: ArrOSt runtime is single-threaded in current milestones.
    unsafe { f(&mut *DRIVER_STATE.0.get()) }
}

fn lerp_i16(a: i16, b: i16, frac: u32) -> i16 {
    let start = i32::from(a);
    let delta = i32::from(b).saturating_sub(start);
    let scaled =
        ((i64::from(delta).saturating_mul(i64::from(frac))).saturating_add(1i64 << 15)) >> 16;
    start
        .saturating_add(scaled.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

fn choose_rate(rates_mask: u64) -> Option<(u8, u32)> {
    let candidates: &[(u8, u32)] = &[
        (RATE_ENUM_44100, 44_100),
        (RATE_ENUM_48000, 48_000),
        (RATE_ENUM_22050, 22_050),
        (RATE_ENUM_11025, 11_025),
    ];
    for (rate_enum, rate_hz) in candidates {
        if (rates_mask & (1u64 << *rate_enum)) != 0 {
            return Some((*rate_enum, *rate_hz));
        }
    }
    None
}

fn floor_pow2(value: u16) -> u16 {
    if value == 0 {
        return 0;
    }
    let mut bit = 1u16 << 15;
    while bit != 0 {
        if (value & bit) != 0 {
            return bit;
        }
        bit >>= 1;
    }
    0
}

fn find_virtio_sound_pci() -> Option<(PciLocation, PciCaps)> {
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
                if vendor != VIRTIO_VENDOR_ID {
                    continue;
                }
                let device_id = pci_read_u16(bus as u8, device as u8, function as u8, 0x02);
                if device_id != VIRTIO_SOUND_MODERN_ID && device_id != VIRTIO_SOUND_TRANSITIONAL_ID
                {
                    continue;
                }

                let location = PciLocation {
                    bus: bus as u8,
                    device: device as u8,
                    function: function as u8,
                    device_id,
                };
                enable_pci_memory_busmaster(&location);
                if let Some(caps) = parse_virtio_pci_caps(&location) {
                    return Some((location, caps));
                }
            }
        }
    }
    None
}

fn enable_pci_memory_busmaster(location: &PciLocation) {
    let command = pci_read_u16(location.bus, location.device, location.function, 0x04) | 0x6;
    pci_write_u16(
        location.bus,
        location.device,
        location.function,
        0x04,
        command,
    );
}

fn parse_virtio_pci_caps(location: &PciLocation) -> Option<PciCaps> {
    let status = pci_read_u16(location.bus, location.device, location.function, 0x06);
    if (status & PCI_STATUS_CAP_LIST) == 0 {
        return None;
    }

    let mut cap_ptr = pci_read_u8(location.bus, location.device, location.function, 0x34) & !0x3;
    let mut common: Option<CapRegion> = None;
    let mut notify: Option<CapRegion> = None;
    let mut notify_multiplier = 0u32;
    let mut isr: Option<CapRegion> = None;
    let mut device: Option<CapRegion> = None;
    let mut guard = 0u16;

    while cap_ptr >= 0x40 && cap_ptr != 0 && guard < 128 {
        guard = guard.saturating_add(1);
        let cap_id = pci_read_u8(location.bus, location.device, location.function, cap_ptr);
        let next = pci_read_u8(
            location.bus,
            location.device,
            location.function,
            cap_ptr + 1,
        );
        if cap_id == PCI_CAP_ID_VENDOR_SPECIFIC {
            let cfg_type = pci_read_u8(
                location.bus,
                location.device,
                location.function,
                cap_ptr + 3,
            );
            let bar = pci_read_u8(
                location.bus,
                location.device,
                location.function,
                cap_ptr + 4,
            );
            let offset = pci_read_u32(
                location.bus,
                location.device,
                location.function,
                cap_ptr + 8,
            );
            let length = pci_read_u32(
                location.bus,
                location.device,
                location.function,
                cap_ptr + 12,
            );
            let region = CapRegion {
                bar,
                offset,
                length,
            };
            match cfg_type {
                VIRTIO_PCI_CAP_COMMON_CFG => common = Some(region),
                VIRTIO_PCI_CAP_NOTIFY_CFG => {
                    notify = Some(region);
                    notify_multiplier = pci_read_u32(
                        location.bus,
                        location.device,
                        location.function,
                        cap_ptr + 16,
                    );
                }
                VIRTIO_PCI_CAP_ISR_CFG => isr = Some(region),
                VIRTIO_PCI_CAP_DEVICE_CFG => device = Some(region),
                _ => {}
            }
        }
        cap_ptr = next & !0x3;
    }

    Some(PciCaps {
        common: common?,
        notify: notify?,
        notify_multiplier: notify_multiplier.max(2),
        isr,
        device: device?,
    })
}

fn map_cap_region(location: &PciLocation, cap: CapRegion) -> Option<*mut u8> {
    if cap.length == 0 {
        return None;
    }
    let bar_base = pci_bar_phys(location, cap.bar)?;
    let phys = bar_base.checked_add(u64::from(cap.offset))?;
    let virt = mem::phys_to_virt(phys)?;
    Some(virt as *mut u8)
}

fn pci_bar_phys(location: &PciLocation, bar: u8) -> Option<u64> {
    if bar >= 6 {
        return None;
    }
    let offset = 0x10u8.saturating_add(bar.saturating_mul(4));
    let low = pci_read_u32(location.bus, location.device, location.function, offset);
    if low == 0 || low == u32::MAX {
        return None;
    }
    if (low & 0x1) != 0 {
        return None;
    }

    let mut base = u64::from(low & !0xF);
    let mem_type = (low >> 1) & 0x3;
    if mem_type == 0x2 {
        if bar >= 5 {
            return None;
        }
        let high = pci_read_u32(
            location.bus,
            location.device,
            location.function,
            offset.saturating_add(4),
        );
        base |= u64::from(high) << 32;
    }
    Some(base)
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

fn pci_read_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let value = pci_read_u32(bus, device, function, offset);
    let shift = ((offset & 0x2) * 8) as u32;
    ((value >> shift) & 0xFFFF) as u16
}

fn pci_read_u8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let value = pci_read_u32(bus, device, function, offset);
    let shift = ((offset & 0x3) * 8) as u32;
    ((value >> shift) & 0xFF) as u8
}

fn pci_write_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let address = pci_address(bus, device, function, offset);
    // SAFETY: x86 PCI config mechanism #1 uses 0xCF8/0xCFC I/O ports.
    unsafe {
        port::outl(PCI_CONFIG_ADDR, address);
        port::outl(PCI_CONFIG_DATA, value);
    }
}

fn pci_write_u16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    let aligned = offset & !0x2;
    let mut dword = pci_read_u32(bus, device, function, aligned);
    let shift = ((offset & 0x2) * 8) as u32;
    dword &= !(0xFFFFu32 << shift);
    dword |= (value as u32) << shift;
    pci_write_u32(bus, device, function, aligned, dword);
}
