// kernel/src/net/mod.rs: M7 virtio-net legacy driver + minimal IPv4/ARP/ICMP/UDP stack.
use crate::arch::x86_64::port;
use crate::mem;
use crate::serial;
use crate::time;
use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::mem::size_of;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering, fence};

const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_NET_TRANSITIONAL_ID: u16 = 0x1000;
const VIRTIO_NET_MODERN_ID: u16 = 0x1041;

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

const RX_QUEUE_INDEX: u16 = 0;
const TX_QUEUE_INDEX: u16 = 1;
const MAX_QUEUE_SIZE: u16 = 256;
const MAX_QUEUE_SIZE_USIZE: usize = MAX_QUEUE_SIZE as usize;
const VRING_ALIGN: usize = 4096;
const MAX_POLL_SPINS: usize = 2_000_000;

const NET_HDR_SIZE: usize = size_of::<VirtioNetHdr>();
const MAX_RX_FRAME: usize = 2048;
const MAX_TX_FRAME: usize = 1536;
const UDP_MAILBOX_CAP: usize = 512;
const CURL_HTTP_BUF: usize = 2048;
const CURL_WAIT_TICKS: u64 = 300;
const DHCP_WAIT_TICKS: u64 = 400;

const LOCAL_IP: [u8; 4] = [10, 0, 2, 15];
const LOCAL_NETMASK: [u8; 4] = [255, 255, 255, 0];
const LOCAL_GATEWAY: [u8; 4] = [10, 0, 2, 2];
const UDP_ECHO_PORT: u16 = 7777;
const PING_IDENTIFIER: u16 = 0xA707;
const UDP_DHCP_SERVER_PORT: u16 = 67;
const UDP_DHCP_CLIENT_PORT: u16 = 68;
const UDP_DNS_PORT: u16 = 53;
const IP_BROADCAST: [u8; 4] = [255, 255, 255, 255];
const IP_ZERO: [u8; 4] = [0, 0, 0, 0];
const MAC_BROADCAST: [u8; 6] = [0xff; 6];
const DHCP_MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];
const DNS_WAIT_TICKS: u64 = 300;

const DHCP_OPT_SUBNET_MASK: u8 = 1;
const DHCP_OPT_ROUTER: u8 = 3;
const DHCP_OPT_DNS: u8 = 6;
const DHCP_OPT_REQ_IP: u8 = 50;
const DHCP_OPT_LEASE_TIME: u8 = 51;
const DHCP_OPT_MSG_TYPE: u8 = 53;
const DHCP_OPT_SERVER_ID: u8 = 54;
const DHCP_OPT_PARAM_REQ_LIST: u8 = 55;
const DHCP_OPT_END: u8 = 255;

const DHCP_MSG_DISCOVER: u8 = 1;
const DHCP_MSG_OFFER: u8 = 2;
const DHCP_MSG_REQUEST: u8 = 3;
const DHCP_MSG_ACK: u8 = 5;

const ETH_TYPE_ARP: u16 = 0x0806;
const ETH_TYPE_IPV4: u16 = 0x0800;
const IP_PROTO_TCP: u8 = 6;
const IP_PROTO_ICMP: u8 = 1;
const IP_PROTO_UDP: u8 = 17;

const TCP_FLAG_FIN: u16 = 0x01;
const TCP_FLAG_SYN: u16 = 0x02;
const TCP_FLAG_RST: u16 = 0x04;
const TCP_FLAG_PSH: u16 = 0x08;
const TCP_FLAG_ACK: u16 = 0x10;

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
const VRING_BYTES: usize = USED_OFFSET + size_of::<VirtqUsed>();

#[repr(C, align(4096))]
struct QueueMemory {
    bytes: [u8; VRING_BYTES],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioNetHdr {
    flags: u8,
    gso_type: u8,
    hdr_len: u16,
    gso_size: u16,
    csum_start: u16,
    csum_offset: u16,
}

#[repr(C, align(16))]
struct RxBuffer {
    hdr: VirtioNetHdr,
    frame: [u8; MAX_RX_FRAME],
}

#[repr(C, align(16))]
struct TxBuffer {
    hdr: VirtioNetHdr,
    frame: [u8; MAX_TX_FRAME],
}

struct QueueMemoryCell(UnsafeCell<QueueMemory>);
struct RxBufferCell(UnsafeCell<RxBuffer>);
struct TxBufferCell(UnsafeCell<TxBuffer>);

// SAFETY: synchronized via `NET_LOCK`.
unsafe impl Sync for QueueMemoryCell {}
// SAFETY: synchronized via `NET_LOCK`.
unsafe impl Sync for RxBufferCell {}
// SAFETY: synchronized via `NET_LOCK`.
unsafe impl Sync for TxBufferCell {}

static RX_QUEUE_MEMORY: QueueMemoryCell = QueueMemoryCell(UnsafeCell::new(QueueMemory {
    bytes: [0; VRING_BYTES],
}));
static TX_QUEUE_MEMORY: QueueMemoryCell = QueueMemoryCell(UnsafeCell::new(QueueMemory {
    bytes: [0; VRING_BYTES],
}));

static RX_BUFFER: RxBufferCell = RxBufferCell(UnsafeCell::new(RxBuffer {
    hdr: VirtioNetHdr {
        flags: 0,
        gso_type: 0,
        hdr_len: 0,
        gso_size: 0,
        csum_start: 0,
        csum_offset: 0,
    },
    frame: [0; MAX_RX_FRAME],
}));

static TX_BUFFER: TxBufferCell = TxBufferCell(UnsafeCell::new(TxBuffer {
    hdr: VirtioNetHdr {
        flags: 0,
        gso_type: 0,
        hdr_len: 0,
        gso_size: 0,
        csum_start: 0,
        csum_offset: 0,
    },
    frame: [0; MAX_TX_FRAME],
}));

#[derive(Clone, Copy)]
struct ArpEntry {
    valid: bool,
    ip: [u8; 4],
    mac: [u8; 6],
}

impl ArpEntry {
    const fn empty() -> Self {
        Self {
            valid: false,
            ip: [0; 4],
            mac: [0; 6],
        }
    }
}

#[derive(Clone, Copy)]
struct PendingPing {
    active: bool,
    ident: u16,
    seq: u16,
    target: [u8; 4],
    start_tick: u64,
    reply_tick: u64,
}

impl PendingPing {
    const fn empty() -> Self {
        Self {
            active: false,
            ident: 0,
            seq: 0,
            target: [0; 4],
            start_tick: 0,
            reply_tick: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct NetStats {
    rx_frames: u64,
    tx_frames: u64,
    rx_arp: u64,
    rx_ipv4: u64,
    rx_icmp: u64,
    rx_udp: u64,
    rx_tcp: u64,
    dhcp_discover: u64,
    dhcp_offer: u64,
    dhcp_ack: u64,
    dns_query: u64,
    dns_answer: u64,
    curl_udp: u64,
    curl_http: u64,
    route_direct: u64,
    route_gateway: u64,
    dropped: u64,
}

impl NetStats {
    const fn new() -> Self {
        Self {
            rx_frames: 0,
            tx_frames: 0,
            rx_arp: 0,
            rx_ipv4: 0,
            rx_icmp: 0,
            rx_udp: 0,
            rx_tcp: 0,
            dhcp_discover: 0,
            dhcp_offer: 0,
            dhcp_ack: 0,
            dns_query: 0,
            dns_answer: 0,
            curl_udp: 0,
            curl_http: 0,
            route_direct: 0,
            route_gateway: 0,
            dropped: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct LastUdp {
    valid: bool,
    src_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    len: usize,
    preview: [u8; 64],
}

impl LastUdp {
    const fn empty() -> Self {
        Self {
            valid: false,
            src_ip: [0; 4],
            src_port: 0,
            dst_port: 0,
            len: 0,
            preview: [0; 64],
        }
    }
}

#[derive(Clone, Copy)]
struct UdpMailbox {
    valid: bool,
    src_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    len: usize,
    data: [u8; UDP_MAILBOX_CAP],
}

impl UdpMailbox {
    const fn empty() -> Self {
        Self {
            valid: false,
            src_ip: [0; 4],
            src_port: 0,
            dst_port: 0,
            len: 0,
            data: [0; UDP_MAILBOX_CAP],
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum IpConfigSource {
    Static,
    Dhcp,
}

impl IpConfigSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Dhcp => "dhcp",
        }
    }
}

#[derive(Clone, Copy)]
struct DhcpOffer {
    valid: bool,
    ip: [u8; 4],
    netmask: [u8; 4],
    gateway: [u8; 4],
    dns: [u8; 4],
    server_id: [u8; 4],
}

impl DhcpOffer {
    const fn empty() -> Self {
        Self {
            valid: false,
            ip: [0; 4],
            netmask: LOCAL_NETMASK,
            gateway: LOCAL_GATEWAY,
            dns: [0; 4],
            server_id: [0; 4],
        }
    }
}

#[derive(Clone, Copy)]
struct PendingHttpCurl {
    active: bool,
    dst_mac: [u8; 6],
    remote_ip: [u8; 4],
    remote_port: u16,
    local_port: u16,
    seq_next: u32,
    ack_next: u32,
    established: bool,
    sent_request: bool,
    finished: bool,
    status_code: u16,
    response_len: usize,
    response: [u8; CURL_HTTP_BUF],
}

impl PendingHttpCurl {
    const fn empty() -> Self {
        Self {
            active: false,
            dst_mac: [0; 6],
            remote_ip: [0; 4],
            remote_port: 0,
            local_port: 0,
            seq_next: 0,
            ack_next: 0,
            established: false,
            sent_request: false,
            finished: false,
            status_code: 0,
            response_len: 0,
            response: [0; CURL_HTTP_BUF],
        }
    }

    fn clear(&mut self) {
        *self = Self::empty();
    }
}

#[derive(Clone, Copy)]
pub struct NetInitReport {
    pub backend: &'static str,
    pub ready: bool,
    pub io_base: u16,
    pub pci_bus: u8,
    pub pci_device: u8,
    pub pci_function: u8,
    pub pci_device_id: u16,
    pub mac: [u8; 6],
    pub ipv4: [u8; 4],
    pub config_source: &'static str,
}

#[derive(Clone, Copy)]
pub struct UdpRxMeta {
    pub src_ip: [u8; 4],
    pub src_port: u16,
    pub dst_port: u16,
    pub len: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum NetError {
    NotReady,
    NotFound,
    QueueUnavailable,
    QueueTooLarge,
    AddressTranslationFailed,
    FrameTooLarge,
    IoTimeout,
    ArpTimeout,
    UdpPayloadTooLarge,
}

impl NetError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotReady => "not_ready",
            Self::NotFound => "not_found",
            Self::QueueUnavailable => "queue_unavailable",
            Self::QueueTooLarge => "queue_too_large",
            Self::AddressTranslationFailed => "address_translation_failed",
            Self::FrameTooLarge => "frame_too_large",
            Self::IoTimeout => "io_timeout",
            Self::ArpTimeout => "arp_timeout",
            Self::UdpPayloadTooLarge => "udp_payload_too_large",
        }
    }
}

#[derive(Clone, Copy)]
struct PciLocation {
    bus: u8,
    device: u8,
    function: u8,
    device_id: u16,
    io_base: u16,
}

struct NetCell(UnsafeCell<NetState>);

// SAFETY: access is serialized through `NET_LOCK`.
unsafe impl Sync for NetCell {}

static NET_LOCK: SpinLock = SpinLock::new();
static NET_STATE: NetCell = NetCell(UnsafeCell::new(NetState::new()));

struct NetState {
    initialized: bool,
    ready: bool,
    io_base: u16,
    pci_bus: u8,
    pci_device: u8,
    pci_function: u8,
    pci_device_id: u16,
    mac: [u8; 6],
    ipv4: [u8; 4],
    netmask: [u8; 4],
    gateway: [u8; 4],
    dns: [u8; 4],
    config_source: IpConfigSource,
    rx_queue_size: u16,
    tx_queue_size: u16,
    rx_last_used: u16,
    rx_avail: u16,
    tx_last_used: u16,
    tx_avail: u16,
    rx_hdr_phys: u64,
    rx_frame_phys: u64,
    tx_hdr_phys: u64,
    tx_frame_phys: u64,
    next_ip_id: u16,
    next_ping_seq: u16,
    arp: [ArpEntry; 8],
    pending_ping: PendingPing,
    stats: NetStats,
    last_udp: LastUdp,
    udp_mailbox: UdpMailbox,
    pending_http: PendingHttpCurl,
    dhcp_xid: u32,
    dhcp_offer: DhcpOffer,
    dhcp_bound: bool,
}

impl NetState {
    const fn new() -> Self {
        Self {
            initialized: false,
            ready: false,
            io_base: 0,
            pci_bus: 0,
            pci_device: 0,
            pci_function: 0,
            pci_device_id: 0,
            mac: [0; 6],
            ipv4: LOCAL_IP,
            netmask: LOCAL_NETMASK,
            gateway: LOCAL_GATEWAY,
            dns: [0; 4],
            config_source: IpConfigSource::Static,
            rx_queue_size: 0,
            tx_queue_size: 0,
            rx_last_used: 0,
            rx_avail: 0,
            tx_last_used: 0,
            tx_avail: 0,
            rx_hdr_phys: 0,
            rx_frame_phys: 0,
            tx_hdr_phys: 0,
            tx_frame_phys: 0,
            next_ip_id: 1,
            next_ping_seq: 1,
            arp: [ArpEntry::empty(); 8],
            pending_ping: PendingPing::empty(),
            stats: NetStats::new(),
            last_udp: LastUdp::empty(),
            udp_mailbox: UdpMailbox::empty(),
            pending_http: PendingHttpCurl::empty(),
            dhcp_xid: 0,
            dhcp_offer: DhcpOffer::empty(),
            dhcp_bound: false,
        }
    }

    fn report(&self) -> NetInitReport {
        NetInitReport {
            backend: if self.ready {
                "virtio-net-legacy"
            } else {
                "none"
            },
            ready: self.ready,
            io_base: self.io_base,
            pci_bus: self.pci_bus,
            pci_device: self.pci_device,
            pci_function: self.pci_function,
            pci_device_id: self.pci_device_id,
            mac: self.mac,
            ipv4: self.ipv4,
            config_source: self.config_source.as_str(),
        }
    }

    fn init(&mut self) -> NetInitReport {
        if self.initialized {
            return self.report();
        }
        match self.try_init() {
            Ok(()) => {
                self.initialized = true;
            }
            Err(err) => {
                self.initialized = true;
                self.ready = false;
                serial::write_fmt(format_args!("Net: init failed ({})\n", err.as_str()));
            }
        }
        self.report()
    }

    fn try_init(&mut self) -> Result<(), NetError> {
        let Some(device) = find_virtio_net_pci() else {
            return Err(NetError::NotFound);
        };
        self.io_base = device.io_base;
        self.pci_bus = device.bus;
        self.pci_device = device.device;
        self.pci_function = device.function;
        self.pci_device_id = device.device_id;

        for i in 0..self.mac.len() {
            self.mac[i] = self.virtio_read_u8(VIRTIO_PCI_DEVICE_CONFIG + i as u16);
        }

        self.virtio_write_status(0);
        self.virtio_write_status(VIRTIO_STATUS_ACK);
        self.virtio_write_status(VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER);
        let _ = self.virtio_read_u32(VIRTIO_PCI_HOST_FEATURES);
        self.virtio_write_u32(VIRTIO_PCI_GUEST_FEATURES, 0);

        self.setup_queue(RX_QUEUE_INDEX)?;
        self.setup_queue(TX_QUEUE_INDEX)?;
        self.setup_buffers_phys()?;
        self.setup_rx_descriptors()?;
        self.post_rx_buffer()?;

        self.virtio_write_status(
            VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
        );
        self.ready = true;

        if !self.try_dhcp()? {
            self.config_source = IpConfigSource::Static;
            serial::write_line("Net: DHCP unavailable, using static 10.0.2.15/24 gw 10.0.2.2");
        } else {
            serial::write_fmt(format_args!(
                "Net: DHCP lease ip={}.{}.{}.{} mask={}.{}.{}.{} gw={}.{}.{}.{} dns={}.{}.{}.{}\n",
                self.ipv4[0],
                self.ipv4[1],
                self.ipv4[2],
                self.ipv4[3],
                self.netmask[0],
                self.netmask[1],
                self.netmask[2],
                self.netmask[3],
                self.gateway[0],
                self.gateway[1],
                self.gateway[2],
                self.gateway[3],
                self.dns[0],
                self.dns[1],
                self.dns[2],
                self.dns[3]
            ));
        }
        Ok(())
    }

    fn setup_queue(&mut self, queue: u16) -> Result<(), NetError> {
        self.virtio_write_u16(VIRTIO_PCI_QUEUE_SEL, queue);
        let size = self.virtio_read_u16(VIRTIO_PCI_QUEUE_NUM);
        if size == 0 {
            self.virtio_write_status(VIRTIO_STATUS_FAILED);
            return Err(NetError::QueueUnavailable);
        }
        if size > MAX_QUEUE_SIZE {
            self.virtio_write_status(VIRTIO_STATUS_FAILED);
            return Err(NetError::QueueTooLarge);
        }

        // SAFETY: `NET_LOCK` serializes exclusive access to queue memory.
        unsafe {
            queue_bytes_mut(queue).fill(0);
        }

        let queue_phys = mem::virt_to_phys(queue_base_ptr(queue) as usize)
            .ok_or(NetError::AddressTranslationFailed)?;
        if !queue_phys.is_multiple_of(VRING_ALIGN as u64) {
            return Err(NetError::AddressTranslationFailed);
        }

        self.virtio_write_u32(VIRTIO_PCI_QUEUE_PFN, (queue_phys >> 12) as u32);
        if self.virtio_read_u32(VIRTIO_PCI_QUEUE_PFN) == 0 {
            self.virtio_write_status(VIRTIO_STATUS_FAILED);
            return Err(NetError::QueueUnavailable);
        }

        if queue == RX_QUEUE_INDEX {
            self.rx_queue_size = size;
            self.rx_last_used = 0;
            self.rx_avail = 0;
        } else {
            self.tx_queue_size = size;
            self.tx_last_used = 0;
            self.tx_avail = 0;
        }
        Ok(())
    }

    fn setup_buffers_phys(&mut self) -> Result<(), NetError> {
        let rx_hdr = rx_hdr_ptr() as usize;
        let rx_frame = rx_frame_ptr() as usize;
        let tx_hdr = tx_hdr_ptr() as usize;
        let tx_frame = tx_frame_ptr() as usize;
        self.rx_hdr_phys = mem::virt_to_phys(rx_hdr).ok_or(NetError::AddressTranslationFailed)?;
        self.rx_frame_phys =
            mem::virt_to_phys(rx_frame).ok_or(NetError::AddressTranslationFailed)?;
        self.tx_hdr_phys = mem::virt_to_phys(tx_hdr).ok_or(NetError::AddressTranslationFailed)?;
        self.tx_frame_phys =
            mem::virt_to_phys(tx_frame).ok_or(NetError::AddressTranslationFailed)?;
        Ok(())
    }

    fn setup_rx_descriptors(&mut self) -> Result<(), NetError> {
        // SAFETY: descriptor memory belongs to queue0 and access is serialized by `NET_LOCK`.
        unsafe {
            let desc = queue_desc_ptr(RX_QUEUE_INDEX);
            write_volatile(
                desc.add(0),
                VirtqDesc {
                    addr: self.rx_hdr_phys,
                    len: NET_HDR_SIZE as u32,
                    flags: VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
                    next: 1,
                },
            );
            write_volatile(
                desc.add(1),
                VirtqDesc {
                    addr: self.rx_frame_phys,
                    len: MAX_RX_FRAME as u32,
                    flags: VIRTQ_DESC_F_WRITE,
                    next: 0,
                },
            );
        }
        Ok(())
    }

    fn post_rx_buffer(&mut self) -> Result<(), NetError> {
        if self.rx_queue_size == 0 {
            return Err(NetError::QueueUnavailable);
        }
        // SAFETY: queue0 avail ring is only modified while holding `NET_LOCK`.
        unsafe {
            let avail = queue_avail_ptr(RX_QUEUE_INDEX);
            let slot = (self.rx_avail % self.rx_queue_size) as usize;
            write_volatile(addr_of_mut!((*avail).ring[slot]), 0);
            fence(Ordering::SeqCst);
            self.rx_avail = self.rx_avail.wrapping_add(1);
            write_volatile(addr_of_mut!((*avail).idx), self.rx_avail);
        }
        self.virtio_write_u16(VIRTIO_PCI_QUEUE_NOTIFY, RX_QUEUE_INDEX);
        Ok(())
    }

    fn poll(&mut self) {
        if !self.ready {
            return;
        }
        while self.poll_rx_once().unwrap_or(false) {}
    }

    fn poll_rx_once(&mut self) -> Result<bool, NetError> {
        // SAFETY: queue0 used ring access is synchronized by `NET_LOCK`.
        unsafe {
            let used = queue_used_ptr(RX_QUEUE_INDEX);
            let used_idx = read_volatile(addr_of!((*used).idx));
            if used_idx == self.rx_last_used {
                return Ok(false);
            }
            let slot = (self.rx_last_used % self.rx_queue_size) as usize;
            let elem = read_volatile(addr_of!((*used).ring[slot]));
            self.rx_last_used = self.rx_last_used.wrapping_add(1);

            let total_len = elem.len as usize;
            let payload_len = total_len.saturating_sub(NET_HDR_SIZE).min(MAX_RX_FRAME);
            let mut frame = [0u8; MAX_RX_FRAME];
            if payload_len > 0 {
                let rx_frame = &(*RX_BUFFER.0.get()).frame;
                frame[..payload_len].copy_from_slice(&rx_frame[..payload_len]);
            }

            self.post_rx_buffer()?;
            self.stats.rx_frames = self.stats.rx_frames.saturating_add(1);
            self.process_frame(&frame[..payload_len])?;
            Ok(true)
        }
    }

    fn process_frame(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if frame.len() < 14 {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        let dst_mac = [frame[0], frame[1], frame[2], frame[3], frame[4], frame[5]];
        let src_mac = [frame[6], frame[7], frame[8], frame[9], frame[10], frame[11]];
        let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
        if dst_mac != self.mac && dst_mac != [0xff; 6] {
            return Ok(());
        }

        match ethertype {
            ETH_TYPE_ARP => {
                self.stats.rx_arp = self.stats.rx_arp.saturating_add(1);
                self.handle_arp(&src_mac, &frame[14..])?;
            }
            ETH_TYPE_IPV4 => {
                self.stats.rx_ipv4 = self.stats.rx_ipv4.saturating_add(1);
                self.handle_ipv4(&src_mac, &frame[14..])?;
            }
            _ => {
                self.stats.dropped = self.stats.dropped.saturating_add(1);
            }
        }
        Ok(())
    }

    fn handle_arp(&mut self, src_mac: &[u8; 6], payload: &[u8]) -> Result<(), NetError> {
        if payload.len() < 28 {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        let htype = u16::from_be_bytes([payload[0], payload[1]]);
        let ptype = u16::from_be_bytes([payload[2], payload[3]]);
        let hlen = payload[4];
        let plen = payload[5];
        if htype != 1 || ptype != ETH_TYPE_IPV4 || hlen != 6 || plen != 4 {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        let oper = u16::from_be_bytes([payload[6], payload[7]]);
        let sender_mac = [
            payload[8],
            payload[9],
            payload[10],
            payload[11],
            payload[12],
            payload[13],
        ];
        let sender_ip = [payload[14], payload[15], payload[16], payload[17]];
        let target_ip = [payload[24], payload[25], payload[26], payload[27]];
        self.learn_arp(sender_ip, sender_mac);

        if oper == 1 && target_ip == self.ipv4 {
            self.send_arp_reply(*src_mac, sender_ip)?;
        }
        Ok(())
    }

    fn handle_ipv4(&mut self, src_mac: &[u8; 6], payload: &[u8]) -> Result<(), NetError> {
        if payload.len() < 20 {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        let version_ihl = payload[0];
        if (version_ihl >> 4) != 4 {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        let ihl = ((version_ihl & 0x0f) as usize) * 4;
        if ihl < 20 || payload.len() < ihl {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        let total_len = u16::from_be_bytes([payload[2], payload[3]]) as usize;
        if total_len < ihl || payload.len() < total_len {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        if checksum(&payload[..ihl]) != 0 {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        let proto = payload[9];
        let src_ip = [payload[12], payload[13], payload[14], payload[15]];
        let dst_ip = [payload[16], payload[17], payload[18], payload[19]];
        self.learn_arp(src_ip, *src_mac);
        if dst_ip != self.ipv4 && dst_ip != IP_BROADCAST {
            return Ok(());
        }
        let body = &payload[ihl..total_len];

        match proto {
            IP_PROTO_ICMP => {
                self.stats.rx_icmp = self.stats.rx_icmp.saturating_add(1);
                self.handle_icmp(*src_mac, src_ip, body)?;
            }
            IP_PROTO_UDP => {
                self.stats.rx_udp = self.stats.rx_udp.saturating_add(1);
                self.handle_udp(*src_mac, src_ip, body)?;
            }
            IP_PROTO_TCP => {
                self.stats.rx_tcp = self.stats.rx_tcp.saturating_add(1);
                self.handle_tcp(*src_mac, src_ip, body)?;
            }
            _ => {
                self.stats.dropped = self.stats.dropped.saturating_add(1);
            }
        }
        Ok(())
    }

    fn handle_icmp(
        &mut self,
        src_mac: [u8; 6],
        src_ip: [u8; 4],
        payload: &[u8],
    ) -> Result<(), NetError> {
        if payload.len() < 8 {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        let icmp_type = payload[0];
        let code = payload[1];
        if code != 0 {
            return Ok(());
        }
        let ident = u16::from_be_bytes([payload[4], payload[5]]);
        let seq = u16::from_be_bytes([payload[6], payload[7]]);

        if icmp_type == 8 {
            let mut reply = [0u8; MAX_TX_FRAME];
            if payload.len() > reply.len() {
                return Err(NetError::FrameTooLarge);
            }
            reply[..payload.len()].copy_from_slice(payload);
            reply[0] = 0;
            reply[2] = 0;
            reply[3] = 0;
            let csum = checksum(&reply[..payload.len()]);
            reply[2..4].copy_from_slice(&csum.to_be_bytes());
            self.send_ipv4_packet(src_mac, src_ip, IP_PROTO_ICMP, &reply[..payload.len()])?;
        } else if icmp_type == 0
            && self.pending_ping.active
            && self.pending_ping.ident == ident
            && self.pending_ping.seq == seq
            && self.pending_ping.target == src_ip
        {
            self.pending_ping.reply_tick = time::ticks();
            self.pending_ping.active = false;
        }
        Ok(())
    }

    fn handle_udp(
        &mut self,
        src_mac: [u8; 6],
        src_ip: [u8; 4],
        payload: &[u8],
    ) -> Result<(), NetError> {
        if payload.len() < 8 {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        let src_port = u16::from_be_bytes([payload[0], payload[1]]);
        let dst_port = u16::from_be_bytes([payload[2], payload[3]]);
        let len = u16::from_be_bytes([payload[4], payload[5]]) as usize;
        if len < 8 || len > payload.len() {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        let data = &payload[8..len];
        if src_port == UDP_DHCP_SERVER_PORT && dst_port == UDP_DHCP_CLIENT_PORT {
            self.handle_dhcp_message(src_ip, data);
            return Ok(());
        }

        self.last_udp.valid = true;
        self.last_udp.src_ip = src_ip;
        self.last_udp.src_port = src_port;
        self.last_udp.dst_port = dst_port;
        self.last_udp.len = data.len();
        self.last_udp.preview.fill(0);
        let preview_len = data.len().min(self.last_udp.preview.len());
        self.last_udp.preview[..preview_len].copy_from_slice(&data[..preview_len]);
        self.udp_mailbox.valid = true;
        self.udp_mailbox.src_ip = src_ip;
        self.udp_mailbox.src_port = src_port;
        self.udp_mailbox.dst_port = dst_port;
        self.udp_mailbox.len = data.len().min(self.udp_mailbox.data.len());
        self.udp_mailbox.data.fill(0);
        self.udp_mailbox.data[..self.udp_mailbox.len]
            .copy_from_slice(&data[..self.udp_mailbox.len]);

        if dst_port == UDP_ECHO_PORT {
            self.send_udp_packet(src_mac, src_ip, src_port, UDP_ECHO_PORT, data)?;
        }
        Ok(())
    }

    fn handle_tcp(
        &mut self,
        _src_mac: [u8; 6],
        src_ip: [u8; 4],
        payload: &[u8],
    ) -> Result<(), NetError> {
        if payload.len() < 20 {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        let src_port = u16::from_be_bytes([payload[0], payload[1]]);
        let dst_port = u16::from_be_bytes([payload[2], payload[3]]);
        let seq = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
        let ack = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
        let data_offset = ((payload[12] >> 4) as usize) * 4;
        if data_offset < 20 || payload.len() < data_offset {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return Ok(());
        }
        let flags = u16::from(payload[13]) & 0x3f;
        let data = &payload[data_offset..];

        if !self.pending_http.active
            || self.pending_http.remote_ip != src_ip
            || self.pending_http.remote_port != src_port
            || self.pending_http.local_port != dst_port
        {
            return Ok(());
        }

        if (flags & TCP_FLAG_RST) != 0 {
            self.pending_http.finished = true;
            return Ok(());
        }

        if (flags & TCP_FLAG_SYN) != 0
            && (flags & TCP_FLAG_ACK) != 0
            && !self.pending_http.established
        {
            if ack == self.pending_http.seq_next {
                self.pending_http.ack_next = seq.wrapping_add(1);
                let _ = self.send_pending_tcp_segment(
                    self.pending_http.seq_next,
                    self.pending_http.ack_next,
                    TCP_FLAG_ACK,
                    &[],
                );
                self.pending_http.established = true;
            }
            return Ok(());
        }

        if !self.pending_http.established {
            return Ok(());
        }

        if !data.is_empty() {
            if seq == self.pending_http.ack_next {
                let available = self
                    .pending_http
                    .response
                    .len()
                    .saturating_sub(self.pending_http.response_len);
                let copy_len = data.len().min(available);
                if copy_len > 0 {
                    let start = self.pending_http.response_len;
                    let end = start + copy_len;
                    self.pending_http.response[start..end].copy_from_slice(&data[..copy_len]);
                    self.pending_http.response_len = end;
                }
                self.pending_http.ack_next =
                    self.pending_http.ack_next.wrapping_add(data.len() as u32);
                if self.pending_http.status_code == 0
                    && let Some(code) = parse_http_status_code(
                        &self.pending_http.response[..self.pending_http.response_len],
                    )
                {
                    self.pending_http.status_code = code;
                }
            }
            let _ = self.send_pending_tcp_segment(
                self.pending_http.seq_next,
                self.pending_http.ack_next,
                TCP_FLAG_ACK,
                &[],
            );
        }

        if (flags & TCP_FLAG_FIN) != 0 {
            self.pending_http.ack_next = self.pending_http.ack_next.wrapping_add(1);
            let _ = self.send_pending_tcp_segment(
                self.pending_http.seq_next,
                self.pending_http.ack_next,
                TCP_FLAG_ACK,
                &[],
            );
            self.pending_http.finished = true;
        }
        Ok(())
    }

    fn send_ping(&mut self, target: [u8; 4]) -> Result<u64, NetError> {
        let mut payload = [0u8; 64];
        let body = b"arr0st-m7-ping";
        payload[..body.len()].copy_from_slice(body);
        let ident = PING_IDENTIFIER;
        let seq = self.next_ping_seq;
        self.next_ping_seq = self.next_ping_seq.wrapping_add(1);

        let mut icmp = [0u8; 96];
        let total = 8 + body.len();
        icmp[0] = 8;
        icmp[1] = 0;
        icmp[4..6].copy_from_slice(&ident.to_be_bytes());
        icmp[6..8].copy_from_slice(&seq.to_be_bytes());
        icmp[8..8 + body.len()].copy_from_slice(&payload[..body.len()]);
        let csum = checksum(&icmp[..total]);
        icmp[2..4].copy_from_slice(&csum.to_be_bytes());

        let next_hop = self.select_next_hop(target);
        let dst_mac = self.resolve_arp(next_hop)?;
        let start = time::ticks();
        self.pending_ping = PendingPing {
            active: true,
            ident,
            seq,
            target,
            start_tick: start,
            reply_tick: 0,
        };
        self.send_ipv4_packet(dst_mac, target, IP_PROTO_ICMP, &icmp[..total])?;

        let timeout_ticks = 300;
        while time::ticks().saturating_sub(start) < timeout_ticks {
            self.poll();
            if !self.pending_ping.active
                && self.pending_ping.ident == ident
                && self.pending_ping.seq == seq
                && self.pending_ping.reply_tick >= self.pending_ping.start_tick
            {
                return Ok(self.pending_ping.reply_tick - self.pending_ping.start_tick);
            }
            spin_loop();
        }
        self.pending_ping.active = false;
        Err(NetError::IoTimeout)
    }

    fn send_udp_shell(
        &mut self,
        target_ip: [u8; 4],
        target_port: u16,
        payload: &[u8],
    ) -> Result<(), NetError> {
        self.send_udp(target_ip, target_port, UDP_ECHO_PORT, payload)
            .map(|_| ())
    }

    fn send_udp(
        &mut self,
        target_ip: [u8; 4],
        target_port: u16,
        src_port: u16,
        payload: &[u8],
    ) -> Result<usize, NetError> {
        if payload.len() > MAX_TX_FRAME.saturating_sub(42) {
            return Err(NetError::UdpPayloadTooLarge);
        }
        let dst_mac = if target_ip == IP_BROADCAST {
            MAC_BROADCAST
        } else {
            let next_hop = self.select_next_hop(target_ip);
            self.resolve_arp(next_hop)?
        };
        let src_port = if src_port == 0 {
            UDP_ECHO_PORT
        } else {
            src_port
        };
        self.send_udp_packet(dst_mac, target_ip, target_port, src_port, payload)?;
        Ok(payload.len())
    }

    fn curl_udp_roundtrip(
        &mut self,
        target_ip: [u8; 4],
        target_port: u16,
        payload: &[u8],
        out: &mut [u8],
    ) -> Result<Option<UdpRxMeta>, NetError> {
        self.udp_mailbox.valid = false;
        self.send_udp(target_ip, target_port, UDP_ECHO_PORT, payload)?;
        let start = time::ticks();
        while time::ticks().saturating_sub(start) < CURL_WAIT_TICKS {
            self.poll();
            if let Some(meta) = self.pop_udp_mailbox(out) {
                return Ok(Some(meta));
            }
            spin_loop();
        }
        Ok(None)
    }

    fn dns_resolve_ipv4(&mut self, host: &str) -> Result<[u8; 4], NetError> {
        let host = host.trim_end_matches('.');
        if host.is_empty() || host.len() > 253 {
            return Err(NetError::NotFound);
        }
        let dns_server = if self.dns != [0; 4] {
            self.dns
        } else if self.gateway != [0; 4] {
            self.gateway
        } else {
            return Err(NetError::NotFound);
        };

        let txid = (self.make_dhcp_xid() as u16).wrapping_add(time::ticks() as u16);
        let src_port = 53000u16.wrapping_add((time::ticks() as u16) & 0x03ff);

        let mut query = [0u8; UDP_MAILBOX_CAP];
        query[0..2].copy_from_slice(&txid.to_be_bytes());
        query[2..4].copy_from_slice(&0x0100u16.to_be_bytes());
        query[4..6].copy_from_slice(&1u16.to_be_bytes());
        query[6..8].copy_from_slice(&0u16.to_be_bytes());
        query[8..10].copy_from_slice(&0u16.to_be_bytes());
        query[10..12].copy_from_slice(&0u16.to_be_bytes());

        let mut idx = 12usize;
        if !encode_dns_name(host, &mut query, &mut idx) {
            return Err(NetError::NotFound);
        }
        if !push_bytes(&mut query, &mut idx, &1u16.to_be_bytes())
            || !push_bytes(&mut query, &mut idx, &1u16.to_be_bytes())
        {
            return Err(NetError::FrameTooLarge);
        }

        self.udp_mailbox.valid = false;
        self.send_udp(dns_server, UDP_DNS_PORT, src_port, &query[..idx])?;
        self.stats.dns_query = self.stats.dns_query.saturating_add(1);

        let start = time::ticks();
        let mut response = [0u8; UDP_MAILBOX_CAP];
        while time::ticks().saturating_sub(start) < DNS_WAIT_TICKS {
            self.poll();
            if let Some(meta) = self.pop_udp_mailbox(&mut response) {
                if meta.src_port != UDP_DNS_PORT {
                    continue;
                }
                if let Some(ip) = parse_dns_a_response(&response[..meta.len], txid) {
                    self.stats.dns_answer = self.stats.dns_answer.saturating_add(1);
                    return Ok(ip);
                }
            }
            spin_loop();
        }
        Err(NetError::IoTimeout)
    }

    fn curl_http_roundtrip(
        &mut self,
        target_ip: [u8; 4],
        target_port: u16,
        path: &str,
    ) -> Result<(usize, u16), NetError> {
        let mut request = [0u8; 512];
        let mut req_len = 0usize;
        if !push_bytes(&mut request, &mut req_len, b"GET ") {
            return Err(NetError::FrameTooLarge);
        }
        if !push_bytes(&mut request, &mut req_len, path.as_bytes()) {
            return Err(NetError::FrameTooLarge);
        }
        if !push_bytes(
            &mut request,
            &mut req_len,
            b" HTTP/1.0\r\nUser-Agent: arr0st-curl/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        ) {
            return Err(NetError::FrameTooLarge);
        }

        let next_hop = self.select_next_hop(target_ip);
        let dst_mac = self.resolve_arp(next_hop)?;
        let local_port = 49152u16.wrapping_add((time::ticks() as u16) & 0x0fff);
        let initial_seq = self.make_dhcp_xid().wrapping_add(0x1234_0000);

        self.pending_http.clear();
        self.pending_http.active = true;
        self.pending_http.dst_mac = dst_mac;
        self.pending_http.remote_ip = target_ip;
        self.pending_http.remote_port = target_port;
        self.pending_http.local_port = local_port;
        self.pending_http.seq_next = initial_seq;

        self.send_pending_tcp_segment(self.pending_http.seq_next, 0, TCP_FLAG_SYN, &[])?;
        self.pending_http.seq_next = self.pending_http.seq_next.wrapping_add(1);

        let start = time::ticks();
        while time::ticks().saturating_sub(start) < CURL_WAIT_TICKS {
            self.poll();

            if self.pending_http.established && !self.pending_http.sent_request {
                self.send_pending_tcp_segment(
                    self.pending_http.seq_next,
                    self.pending_http.ack_next,
                    TCP_FLAG_ACK | TCP_FLAG_PSH,
                    &request[..req_len],
                )?;
                self.pending_http.seq_next =
                    self.pending_http.seq_next.wrapping_add(req_len as u32);
                self.pending_http.sent_request = true;
            }

            if self.pending_http.finished {
                break;
            }
            spin_loop();
        }

        let got_any = self.pending_http.response_len > 0;
        let status = self.pending_http.status_code;
        let response_len = self.pending_http.response_len;
        if !self.pending_http.finished {
            let _ = self.send_pending_tcp_segment(
                self.pending_http.seq_next,
                self.pending_http.ack_next,
                TCP_FLAG_RST | TCP_FLAG_ACK,
                &[],
            );
        }
        self.pending_http.clear();

        if !got_any {
            return Err(NetError::IoTimeout);
        }
        Ok((response_len, status))
    }

    fn send_pending_tcp_segment(
        &mut self,
        seq: u32,
        ack: u32,
        flags: u16,
        payload: &[u8],
    ) -> Result<(), NetError> {
        if payload.len() > MAX_TX_FRAME.saturating_sub(40) {
            return Err(NetError::FrameTooLarge);
        }
        if !self.pending_http.active {
            return Err(NetError::NotReady);
        }
        let mut segment = [0u8; MAX_TX_FRAME];
        segment[0..2].copy_from_slice(&self.pending_http.local_port.to_be_bytes());
        segment[2..4].copy_from_slice(&self.pending_http.remote_port.to_be_bytes());
        segment[4..8].copy_from_slice(&seq.to_be_bytes());
        segment[8..12].copy_from_slice(&ack.to_be_bytes());
        segment[12] = 5u8 << 4;
        segment[13] = (flags & 0x3f) as u8;
        segment[14..16].copy_from_slice(&4096u16.to_be_bytes());
        segment[16..18].copy_from_slice(&0u16.to_be_bytes());
        segment[18..20].copy_from_slice(&0u16.to_be_bytes());
        segment[20..20 + payload.len()].copy_from_slice(payload);
        let tcp_len = 20 + payload.len();
        let checksum = tcp_checksum(self.ipv4, self.pending_http.remote_ip, &segment[..tcp_len]);
        segment[16..18].copy_from_slice(&checksum.to_be_bytes());
        self.send_ipv4_packet_with_src(
            self.pending_http.dst_mac,
            self.pending_http.remote_ip,
            self.ipv4,
            IP_PROTO_TCP,
            &segment[..tcp_len],
        )
    }

    fn send_udp_packet(
        &mut self,
        dst_mac: [u8; 6],
        dst_ip: [u8; 4],
        dst_port: u16,
        src_port: u16,
        payload: &[u8],
    ) -> Result<(), NetError> {
        self.send_udp_packet_with_src(dst_mac, dst_ip, dst_port, src_port, self.ipv4, payload)
    }

    fn send_udp_packet_with_src(
        &mut self,
        dst_mac: [u8; 6],
        dst_ip: [u8; 4],
        dst_port: u16,
        src_port: u16,
        src_ip: [u8; 4],
        payload: &[u8],
    ) -> Result<(), NetError> {
        let mut udp = [0u8; MAX_TX_FRAME];
        let udp_len = 8 + payload.len();
        udp[0..2].copy_from_slice(&src_port.to_be_bytes());
        udp[2..4].copy_from_slice(&dst_port.to_be_bytes());
        udp[4..6].copy_from_slice(&(udp_len as u16).to_be_bytes());
        udp[6..8].copy_from_slice(&0u16.to_be_bytes());
        udp[8..8 + payload.len()].copy_from_slice(payload);
        self.send_ipv4_packet_with_src(dst_mac, dst_ip, src_ip, IP_PROTO_UDP, &udp[..udp_len])
    }

    fn send_ipv4_packet(
        &mut self,
        dst_mac: [u8; 6],
        dst_ip: [u8; 4],
        proto: u8,
        payload: &[u8],
    ) -> Result<(), NetError> {
        self.send_ipv4_packet_with_src(dst_mac, dst_ip, self.ipv4, proto, payload)
    }

    fn send_ipv4_packet_with_src(
        &mut self,
        dst_mac: [u8; 6],
        dst_ip: [u8; 4],
        src_ip: [u8; 4],
        proto: u8,
        payload: &[u8],
    ) -> Result<(), NetError> {
        let total_len = 20 + payload.len();
        if total_len > MAX_TX_FRAME.saturating_sub(14) {
            return Err(NetError::FrameTooLarge);
        }

        let mut frame = [0u8; MAX_TX_FRAME];
        frame[0..6].copy_from_slice(&dst_mac);
        frame[6..12].copy_from_slice(&self.mac);
        frame[12..14].copy_from_slice(&ETH_TYPE_IPV4.to_be_bytes());

        let ip = &mut frame[14..14 + 20];
        ip[0] = 0x45;
        ip[1] = 0;
        ip[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
        ip[4..6].copy_from_slice(&self.next_ip_id.to_be_bytes());
        self.next_ip_id = self.next_ip_id.wrapping_add(1);
        ip[6..8].copy_from_slice(&0x4000u16.to_be_bytes());
        ip[8] = 64;
        ip[9] = proto;
        ip[10..12].copy_from_slice(&0u16.to_be_bytes());
        ip[12..16].copy_from_slice(&src_ip);
        ip[16..20].copy_from_slice(&dst_ip);
        let ip_csum = checksum(ip);
        ip[10..12].copy_from_slice(&ip_csum.to_be_bytes());

        frame[34..34 + payload.len()].copy_from_slice(payload);
        self.transmit_frame(&frame[..14 + total_len])
    }

    fn transmit_frame(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if !self.ready {
            return Err(NetError::NotReady);
        }
        if frame.len() > MAX_TX_FRAME {
            return Err(NetError::FrameTooLarge);
        }

        // SAFETY: `NET_LOCK` serializes access to shared TX buffer.
        unsafe {
            let tx = &mut *TX_BUFFER.0.get();
            tx.hdr.flags = 0;
            tx.hdr.gso_type = 0;
            tx.hdr.hdr_len = 0;
            tx.hdr.gso_size = 0;
            tx.hdr.csum_start = 0;
            tx.hdr.csum_offset = 0;
            tx.frame[..frame.len()].copy_from_slice(frame);
        }

        // SAFETY: queue1 memory belongs to TX queue and is serialized by `NET_LOCK`.
        unsafe {
            let desc = queue_desc_ptr(TX_QUEUE_INDEX);
            write_volatile(
                desc.add(0),
                VirtqDesc {
                    addr: self.tx_hdr_phys,
                    len: NET_HDR_SIZE as u32,
                    flags: VIRTQ_DESC_F_NEXT,
                    next: 1,
                },
            );
            write_volatile(
                desc.add(1),
                VirtqDesc {
                    addr: self.tx_frame_phys,
                    len: frame.len() as u32,
                    flags: 0,
                    next: 0,
                },
            );

            let avail = queue_avail_ptr(TX_QUEUE_INDEX);
            let slot = (self.tx_avail % self.tx_queue_size) as usize;
            write_volatile(addr_of_mut!((*avail).ring[slot]), 0);
            fence(Ordering::SeqCst);
            self.tx_avail = self.tx_avail.wrapping_add(1);
            write_volatile(addr_of_mut!((*avail).idx), self.tx_avail);
            fence(Ordering::SeqCst);
        }

        self.virtio_write_u16(VIRTIO_PCI_QUEUE_NOTIFY, TX_QUEUE_INDEX);

        let expected = self.tx_last_used.wrapping_add(1);
        let mut spins = 0usize;
        loop {
            // SAFETY: queue1 used ring is accessed while `NET_LOCK` is held.
            let observed =
                unsafe { read_volatile(addr_of!((*queue_used_ptr(TX_QUEUE_INDEX)).idx)) };
            if observed == expected {
                self.tx_last_used = expected;
                self.stats.tx_frames = self.stats.tx_frames.saturating_add(1);
                break;
            }
            if spins >= MAX_POLL_SPINS {
                let _ = self.virtio_read_u8(VIRTIO_PCI_ISR);
                return Err(NetError::IoTimeout);
            }
            spins = spins.saturating_add(1);
            spin_loop();
        }
        Ok(())
    }

    fn send_arp_request(&mut self, target_ip: [u8; 4]) -> Result<(), NetError> {
        let mut payload = [0u8; 28];
        payload[0..2].copy_from_slice(&1u16.to_be_bytes());
        payload[2..4].copy_from_slice(&ETH_TYPE_IPV4.to_be_bytes());
        payload[4] = 6;
        payload[5] = 4;
        payload[6..8].copy_from_slice(&1u16.to_be_bytes());
        payload[8..14].copy_from_slice(&self.mac);
        payload[14..18].copy_from_slice(&self.ipv4);
        payload[18..24].copy_from_slice(&[0; 6]);
        payload[24..28].copy_from_slice(&target_ip);

        let mut frame = [0u8; 64];
        frame[0..6].copy_from_slice(&[0xff; 6]);
        frame[6..12].copy_from_slice(&self.mac);
        frame[12..14].copy_from_slice(&ETH_TYPE_ARP.to_be_bytes());
        frame[14..42].copy_from_slice(&payload);
        self.transmit_frame(&frame[..42])
    }

    fn send_arp_reply(&mut self, target_mac: [u8; 6], target_ip: [u8; 4]) -> Result<(), NetError> {
        let mut payload = [0u8; 28];
        payload[0..2].copy_from_slice(&1u16.to_be_bytes());
        payload[2..4].copy_from_slice(&ETH_TYPE_IPV4.to_be_bytes());
        payload[4] = 6;
        payload[5] = 4;
        payload[6..8].copy_from_slice(&2u16.to_be_bytes());
        payload[8..14].copy_from_slice(&self.mac);
        payload[14..18].copy_from_slice(&self.ipv4);
        payload[18..24].copy_from_slice(&target_mac);
        payload[24..28].copy_from_slice(&target_ip);

        let mut frame = [0u8; 64];
        frame[0..6].copy_from_slice(&target_mac);
        frame[6..12].copy_from_slice(&self.mac);
        frame[12..14].copy_from_slice(&ETH_TYPE_ARP.to_be_bytes());
        frame[14..42].copy_from_slice(&payload);
        self.transmit_frame(&frame[..42])
    }

    fn try_dhcp(&mut self) -> Result<bool, NetError> {
        self.config_source = IpConfigSource::Static;
        self.dhcp_bound = false;
        self.dhcp_offer = DhcpOffer::empty();
        self.dhcp_xid = self.make_dhcp_xid();

        self.send_dhcp_discover(self.dhcp_xid)?;
        self.stats.dhcp_discover = self.stats.dhcp_discover.saturating_add(1);

        let discover_start = time::ticks();
        while time::ticks().saturating_sub(discover_start) < DHCP_WAIT_TICKS {
            self.poll();
            if self.dhcp_offer.valid {
                break;
            }
            spin_loop();
        }

        if !self.dhcp_offer.valid {
            self.dhcp_xid = 0;
            return Ok(false);
        }

        let offer = self.dhcp_offer;
        self.send_dhcp_request(self.dhcp_xid, offer)?;

        let request_start = time::ticks();
        while time::ticks().saturating_sub(request_start) < DHCP_WAIT_TICKS {
            self.poll();
            if self.dhcp_bound {
                return Ok(true);
            }
            spin_loop();
        }

        self.dhcp_xid = 0;
        self.dhcp_offer = DhcpOffer::empty();
        Ok(false)
    }

    fn make_dhcp_xid(&self) -> u32 {
        let mac_part = ((self.mac[2] as u32) << 24)
            | ((self.mac[3] as u32) << 16)
            | ((self.mac[4] as u32) << 8)
            | (self.mac[5] as u32);
        let tick_part = time::ticks() as u32;
        tick_part ^ mac_part ^ 0xA770_5D00
    }

    fn send_dhcp_discover(&mut self, xid: u32) -> Result<(), NetError> {
        let mut packet = [0u8; 320];
        packet[0] = 1;
        packet[1] = 1;
        packet[2] = 6;
        packet[3] = 0;
        packet[4..8].copy_from_slice(&xid.to_be_bytes());
        packet[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
        packet[28..34].copy_from_slice(&self.mac);
        packet[236..240].copy_from_slice(&DHCP_MAGIC_COOKIE);
        let mut idx = 240usize;
        packet[idx] = DHCP_OPT_MSG_TYPE;
        packet[idx + 1] = 1;
        packet[idx + 2] = DHCP_MSG_DISCOVER;
        idx += 3;
        packet[idx] = DHCP_OPT_PARAM_REQ_LIST;
        packet[idx + 1] = 3;
        packet[idx + 2] = DHCP_OPT_SUBNET_MASK;
        packet[idx + 3] = DHCP_OPT_ROUTER;
        packet[idx + 4] = DHCP_OPT_DNS;
        idx += 5;
        packet[idx] = DHCP_OPT_END;
        idx += 1;
        self.send_udp_packet_with_src(
            MAC_BROADCAST,
            IP_BROADCAST,
            UDP_DHCP_SERVER_PORT,
            UDP_DHCP_CLIENT_PORT,
            IP_ZERO,
            &packet[..idx],
        )
    }

    fn send_dhcp_request(&mut self, xid: u32, offer: DhcpOffer) -> Result<(), NetError> {
        let mut packet = [0u8; 320];
        packet[0] = 1;
        packet[1] = 1;
        packet[2] = 6;
        packet[3] = 0;
        packet[4..8].copy_from_slice(&xid.to_be_bytes());
        packet[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
        packet[28..34].copy_from_slice(&self.mac);
        packet[236..240].copy_from_slice(&DHCP_MAGIC_COOKIE);
        let mut idx = 240usize;
        packet[idx] = DHCP_OPT_MSG_TYPE;
        packet[idx + 1] = 1;
        packet[idx + 2] = DHCP_MSG_REQUEST;
        idx += 3;
        packet[idx] = DHCP_OPT_REQ_IP;
        packet[idx + 1] = 4;
        packet[idx + 2..idx + 6].copy_from_slice(&offer.ip);
        idx += 6;
        packet[idx] = DHCP_OPT_SERVER_ID;
        packet[idx + 1] = 4;
        packet[idx + 2..idx + 6].copy_from_slice(&offer.server_id);
        idx += 6;
        packet[idx] = DHCP_OPT_PARAM_REQ_LIST;
        packet[idx + 1] = 3;
        packet[idx + 2] = DHCP_OPT_SUBNET_MASK;
        packet[idx + 3] = DHCP_OPT_ROUTER;
        packet[idx + 4] = DHCP_OPT_DNS;
        idx += 5;
        packet[idx] = DHCP_OPT_END;
        idx += 1;
        self.send_udp_packet_with_src(
            MAC_BROADCAST,
            IP_BROADCAST,
            UDP_DHCP_SERVER_PORT,
            UDP_DHCP_CLIENT_PORT,
            IP_ZERO,
            &packet[..idx],
        )
    }

    fn handle_dhcp_message(&mut self, src_ip: [u8; 4], payload: &[u8]) {
        if payload.len() < 240 {
            return;
        }
        if payload[0] != 2 {
            return;
        }
        let xid = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
        if self.dhcp_xid == 0 || xid != self.dhcp_xid {
            return;
        }
        if payload[236..240] != DHCP_MAGIC_COOKIE {
            return;
        }

        let yiaddr = [payload[16], payload[17], payload[18], payload[19]];
        let mut msg_type = 0u8;
        let mut netmask = [0u8; 4];
        let mut gateway = [0u8; 4];
        let mut dns = [0u8; 4];
        let mut server_id = [0u8; 4];
        let mut idx = 240usize;

        while idx < payload.len() {
            let code = payload[idx];
            idx = idx.saturating_add(1);
            if code == 0 {
                continue;
            }
            if code == DHCP_OPT_END {
                break;
            }
            if idx >= payload.len() {
                break;
            }
            let opt_len = payload[idx] as usize;
            idx = idx.saturating_add(1);
            if idx.saturating_add(opt_len) > payload.len() {
                break;
            }
            let value = &payload[idx..idx + opt_len];
            match code {
                DHCP_OPT_MSG_TYPE if opt_len == 1 => {
                    msg_type = value[0];
                }
                DHCP_OPT_SUBNET_MASK if opt_len >= 4 => {
                    netmask.copy_from_slice(&value[..4]);
                }
                DHCP_OPT_ROUTER if opt_len >= 4 => {
                    gateway.copy_from_slice(&value[..4]);
                }
                DHCP_OPT_DNS if opt_len >= 4 => {
                    dns.copy_from_slice(&value[..4]);
                }
                DHCP_OPT_SERVER_ID if opt_len >= 4 => {
                    server_id.copy_from_slice(&value[..4]);
                }
                DHCP_OPT_LEASE_TIME if opt_len == 4 => {}
                _ => {}
            }
            idx = idx.saturating_add(opt_len);
        }

        if server_id == [0; 4] {
            server_id = src_ip;
        }
        if msg_type == DHCP_MSG_OFFER {
            self.stats.dhcp_offer = self.stats.dhcp_offer.saturating_add(1);
            self.dhcp_offer = DhcpOffer {
                valid: true,
                ip: yiaddr,
                netmask: if netmask == [0; 4] {
                    LOCAL_NETMASK
                } else {
                    netmask
                },
                gateway: if gateway == [0; 4] {
                    LOCAL_GATEWAY
                } else {
                    gateway
                },
                dns,
                server_id,
            };
            return;
        }

        if msg_type != DHCP_MSG_ACK {
            return;
        }

        let fallback = self.dhcp_offer;
        let lease_ip = if yiaddr != [0; 4] {
            yiaddr
        } else if fallback.valid {
            fallback.ip
        } else {
            self.ipv4
        };
        let lease_mask = if netmask != [0; 4] {
            netmask
        } else if fallback.valid {
            fallback.netmask
        } else {
            LOCAL_NETMASK
        };
        let lease_gateway = if gateway != [0; 4] {
            gateway
        } else if fallback.valid {
            fallback.gateway
        } else {
            LOCAL_GATEWAY
        };
        let lease_dns = if dns != [0; 4] {
            dns
        } else if fallback.valid {
            fallback.dns
        } else {
            [0; 4]
        };
        self.ipv4 = lease_ip;
        self.netmask = lease_mask;
        self.gateway = lease_gateway;
        self.dns = lease_dns;
        self.config_source = IpConfigSource::Dhcp;
        self.dhcp_bound = true;
        self.dhcp_xid = 0;
        self.dhcp_offer = DhcpOffer::empty();
        self.stats.dhcp_ack = self.stats.dhcp_ack.saturating_add(1);
    }

    fn select_next_hop(&mut self, dst_ip: [u8; 4]) -> [u8; 4] {
        if dst_ip == self.ipv4 || self.in_same_subnet(dst_ip) || self.gateway == [0; 4] {
            self.stats.route_direct = self.stats.route_direct.saturating_add(1);
            return dst_ip;
        }
        self.stats.route_gateway = self.stats.route_gateway.saturating_add(1);
        self.gateway
    }

    fn in_same_subnet(&self, other: [u8; 4]) -> bool {
        (self.ipv4[0] & self.netmask[0]) == (other[0] & self.netmask[0])
            && (self.ipv4[1] & self.netmask[1]) == (other[1] & self.netmask[1])
            && (self.ipv4[2] & self.netmask[2]) == (other[2] & self.netmask[2])
            && (self.ipv4[3] & self.netmask[3]) == (other[3] & self.netmask[3])
    }

    fn learn_arp(&mut self, ip: [u8; 4], mac: [u8; 6]) {
        if ip == [0; 4] || mac == [0; 6] {
            return;
        }
        for entry in &mut self.arp {
            if entry.valid && entry.ip == ip {
                entry.mac = mac;
                return;
            }
        }
        for entry in &mut self.arp {
            if !entry.valid {
                entry.valid = true;
                entry.ip = ip;
                entry.mac = mac;
                return;
            }
        }
        self.arp[0] = ArpEntry {
            valid: true,
            ip,
            mac,
        };
    }

    fn lookup_arp(&self, ip: [u8; 4]) -> Option<[u8; 6]> {
        self.arp
            .iter()
            .find(|entry| entry.valid && entry.ip == ip)
            .map(|entry| entry.mac)
    }

    fn resolve_arp(&mut self, target_ip: [u8; 4]) -> Result<[u8; 6], NetError> {
        if target_ip == self.ipv4 {
            return Ok(self.mac);
        }
        if let Some(mac) = self.lookup_arp(target_ip) {
            return Ok(mac);
        }
        self.send_arp_request(target_ip)?;
        let start = time::ticks();
        while time::ticks().saturating_sub(start) < 200 {
            self.poll();
            if let Some(mac) = self.lookup_arp(target_ip) {
                return Ok(mac);
            }
            spin_loop();
        }
        Err(NetError::ArpTimeout)
    }

    fn pop_udp_mailbox(&mut self, dst: &mut [u8]) -> Option<UdpRxMeta> {
        if !self.udp_mailbox.valid {
            return None;
        }

        let copy_len = self.udp_mailbox.len.min(dst.len());
        if copy_len > 0 {
            dst[..copy_len].copy_from_slice(&self.udp_mailbox.data[..copy_len]);
        }

        let meta = UdpRxMeta {
            src_ip: self.udp_mailbox.src_ip,
            src_port: self.udp_mailbox.src_port,
            dst_port: self.udp_mailbox.dst_port,
            len: copy_len,
        };
        self.udp_mailbox.valid = false;
        Some(meta)
    }

    fn virtio_read_u8(&self, offset: u16) -> u8 {
        // SAFETY: device I/O port range is validated during PCI discovery.
        unsafe { port::inb(self.io_base.saturating_add(offset)) }
    }

    fn virtio_write_u8(&self, offset: u16, value: u8) {
        // SAFETY: device I/O port range is validated during PCI discovery.
        unsafe { port::outb(self.io_base.saturating_add(offset), value) }
    }

    fn virtio_read_u16(&self, offset: u16) -> u16 {
        // SAFETY: device I/O port range is validated during PCI discovery.
        unsafe { port::inw(self.io_base.saturating_add(offset)) }
    }

    fn virtio_write_u16(&self, offset: u16, value: u16) {
        // SAFETY: device I/O port range is validated during PCI discovery.
        unsafe { port::outw(self.io_base.saturating_add(offset), value) }
    }

    fn virtio_read_u32(&self, offset: u16) -> u32 {
        // SAFETY: device I/O port range is validated during PCI discovery.
        unsafe { port::inl(self.io_base.saturating_add(offset)) }
    }

    fn virtio_write_u32(&self, offset: u16, value: u32) {
        // SAFETY: device I/O port range is validated during PCI discovery.
        unsafe { port::outl(self.io_base.saturating_add(offset), value) }
    }

    fn virtio_write_status(&self, status: u8) {
        self.virtio_write_u8(VIRTIO_PCI_STATUS, status);
    }
}

pub fn init() -> NetInitReport {
    with_net_mut(|state| state.init())
}

pub fn poll() {
    with_net_mut(|state| state.poll());
}

pub fn log_info() {
    with_net(|state| {
        if !state.ready {
            serial::write_line("net: backend=none status=unavailable");
            return;
        }
        serial::write_fmt(format_args!(
            "net: backend=virtio-net-legacy cfg={} io={:#06x} pci={:02x}:{:02x}.{} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} ip={}.{}.{}.{} gw={}.{}.{}.{} mask={}.{}.{}.{} dns={}.{}.{}.{} rx={} tx={} arp={} ipv4={} icmp={} udp={} tcp={} dhcp_discover={} dhcp_offer={} dhcp_ack={} dns_query={} dns_answer={} curl_udp={} curl_http={} route_direct={} route_gw={} drop={}\n",
            state.config_source.as_str(),
            state.io_base,
            state.pci_bus,
            state.pci_device,
            state.pci_function,
            state.mac[0],
            state.mac[1],
            state.mac[2],
            state.mac[3],
            state.mac[4],
            state.mac[5],
            state.ipv4[0],
            state.ipv4[1],
            state.ipv4[2],
            state.ipv4[3],
            state.gateway[0],
            state.gateway[1],
            state.gateway[2],
            state.gateway[3],
            state.netmask[0],
            state.netmask[1],
            state.netmask[2],
            state.netmask[3],
            state.dns[0],
            state.dns[1],
            state.dns[2],
            state.dns[3],
            state.stats.rx_frames,
            state.stats.tx_frames,
            state.stats.rx_arp,
            state.stats.rx_ipv4,
            state.stats.rx_icmp,
            state.stats.rx_udp,
            state.stats.rx_tcp,
            state.stats.dhcp_discover,
            state.stats.dhcp_offer,
            state.stats.dhcp_ack,
            state.stats.dns_query,
            state.stats.dns_answer,
            state.stats.curl_udp,
            state.stats.curl_http,
            state.stats.route_direct,
            state.stats.route_gateway,
            state.stats.dropped
        ));
    });
}

pub fn ping_to_serial(ip_text: &str) {
    let Some(target) = parse_ipv4(ip_text) else {
        serial::write_line("ping: invalid ip (usage: ping <a.b.c.d>)");
        return;
    };
    match with_net_mut(|state| state.send_ping(target)) {
        Ok(rtt_ticks) => serial::write_fmt(format_args!(
            "ping: reply from {}.{}.{}.{} time={} ticks ({} ms)\n",
            target[0],
            target[1],
            target[2],
            target[3],
            rtt_ticks,
            rtt_ticks.saturating_mul(10)
        )),
        Err(err) => serial::write_fmt(format_args!("ping: failed ({})\n", err.as_str())),
    }
}

pub fn curl_to_serial(spec: &str) {
    if let Some((target, port, payload)) = parse_udp_url(spec) {
        with_net_mut(|state| {
            state.stats.curl_udp = state.stats.curl_udp.saturating_add(1);
        });
        curl_udp_to_serial_ip(target, port, payload);
        return;
    }
    if let Some((host, port, path)) = parse_http_url(spec) {
        with_net_mut(|state| {
            state.stats.curl_http = state.stats.curl_http.saturating_add(1);
        });
        let target = match parse_ipv4(host) {
            Some(ip) => ip,
            None => match with_net_mut(|state| state.dns_resolve_ipv4(host)) {
                Ok(ip) => {
                    serial::write_fmt(format_args!(
                        "curl: dns {} -> {}.{}.{}.{}\n",
                        host, ip[0], ip[1], ip[2], ip[3]
                    ));
                    ip
                }
                Err(err) => {
                    serial::write_fmt(format_args!("curl: dns failed ({})\n", err.as_str()));
                    return;
                }
            },
        };
        let path = if path.is_empty() { "/" } else { path };
        match with_net_mut(|state| state.curl_http_roundtrip(target, port, path)) {
            Ok((bytes, status)) => {
                if status != 0 {
                    serial::write_fmt(format_args!(
                        "curl: http {}.{}.{}.{}:{}{} status={} bytes={}\n",
                        target[0], target[1], target[2], target[3], port, path, status, bytes
                    ));
                } else {
                    serial::write_fmt(format_args!(
                        "curl: http {}.{}.{}.{}:{}{} bytes={}\n",
                        target[0], target[1], target[2], target[3], port, path, bytes
                    ));
                }
            }
            Err(err) => serial::write_fmt(format_args!("curl: http failed ({})\n", err.as_str())),
        }
        return;
    }

    let mut parts = spec.trim().splitn(3, ' ');
    let ip = parts.next().unwrap_or_default();
    let port = parts.next().and_then(|value| value.parse::<u16>().ok());
    let payload = parts.next().unwrap_or_default();
    match (parse_ipv4(ip), port, payload.is_empty()) {
        (Some(target), Some(port), false) => {
            with_net_mut(|state| {
                state.stats.curl_udp = state.stats.curl_udp.saturating_add(1);
            });
            curl_udp_to_serial_ip(target, port, payload)
        }
        _ => serial::write_line(
            "usage: curl <ip> <port> <text> | curl udp://<ip>:<port>/<payload> | curl http://<ip>[:port]/<path>",
        ),
    }
}

pub fn udp_send_to_serial(ip_text: &str, port: u16, payload: &str) {
    let Some(target) = parse_ipv4(ip_text) else {
        serial::write_line("udp: invalid ip");
        return;
    };
    match with_net_mut(|state| state.send_udp_shell(target, port, payload.as_bytes())) {
        Ok(()) => serial::write_fmt(format_args!(
            "udp: sent {} bytes to {}.{}.{}.{}:{}\n",
            payload.len(),
            target[0],
            target[1],
            target[2],
            target[3],
            port
        )),
        Err(err) => serial::write_fmt(format_args!("udp: failed ({})\n", err.as_str())),
    }
}

fn curl_udp_to_serial_ip(target: [u8; 4], port: u16, payload: &str) {
    let mut response = [0u8; UDP_MAILBOX_CAP];
    match with_net_mut(|state| {
        state.curl_udp_roundtrip(target, port, payload.as_bytes(), &mut response)
    }) {
        Ok(Some(meta)) => {
            let body = core::str::from_utf8(&response[..meta.len]).unwrap_or("<binary>");
            serial::write_fmt(format_args!(
                "curl: udp {}.{}.{}.{}:{} -> {} bytes from {}.{}.{}.{}:{} `{}`\n",
                target[0],
                target[1],
                target[2],
                target[3],
                port,
                meta.len,
                meta.src_ip[0],
                meta.src_ip[1],
                meta.src_ip[2],
                meta.src_ip[3],
                meta.src_port,
                body
            ));
        }
        Ok(None) => serial::write_line("curl: timeout waiting response"),
        Err(err) => serial::write_fmt(format_args!("curl: failed ({})\n", err.as_str())),
    }
}

pub fn udp_send(
    target_ip: [u8; 4],
    target_port: u16,
    src_port: u16,
    payload: &[u8],
) -> Result<usize, NetError> {
    with_net_mut(|state| state.send_udp(target_ip, target_port, src_port, payload))
}

pub fn udp_recv(buffer: &mut [u8]) -> Result<Option<UdpRxMeta>, NetError> {
    with_net_mut(|state| {
        if !state.ready {
            return Err(NetError::NotReady);
        }
        Ok(state.pop_udp_mailbox(buffer))
    })
}

pub fn log_last_udp() {
    with_net(|state| {
        if !state.last_udp.valid {
            serial::write_line("udp: no packets received");
            return;
        }
        let preview_len = state.last_udp.len.min(state.last_udp.preview.len());
        let preview =
            core::str::from_utf8(&state.last_udp.preview[..preview_len]).unwrap_or("<binary>");
        serial::write_fmt(format_args!(
            "udp: last src={}.{}.{}.{}:{} dst_port={} len={} preview=`{}`\n",
            state.last_udp.src_ip[0],
            state.last_udp.src_ip[1],
            state.last_udp.src_ip[2],
            state.last_udp.src_ip[3],
            state.last_udp.src_port,
            state.last_udp.dst_port,
            state.last_udp.len,
            preview
        ));
    });
}

pub fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut idx = 0usize;
    for part in text.split('.') {
        if idx >= 4 || part.is_empty() {
            return None;
        }
        let value = part.parse::<u8>().ok()?;
        out[idx] = value;
        idx = idx.saturating_add(1);
    }
    if idx != 4 {
        return None;
    }
    Some(out)
}

fn parse_udp_url(spec: &str) -> Option<([u8; 4], u16, &str)> {
    let rest = spec.strip_prefix("udp://")?;
    let slash = rest.find('/')?;
    let host_port = &rest[..slash];
    let payload = &rest[slash + 1..];
    if payload.is_empty() {
        return None;
    }
    let (host, port) = parse_host_port(host_port, None)?;
    let ip = parse_ipv4(host)?;
    Some((ip, port, payload))
}

fn parse_http_url(spec: &str) -> Option<(&str, u16, &str)> {
    let rest = spec.strip_prefix("http://")?;
    let (host_port, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let (host, port) = parse_host_port(host_port, Some(80))?;
    Some((host, port, path))
}

fn parse_host_port(host_port: &str, default_port: Option<u16>) -> Option<(&str, u16)> {
    if host_port.is_empty() {
        return None;
    }
    if let Some((host, port_text)) = host_port.rsplit_once(':') {
        if host.is_empty() {
            return None;
        }
        let port = port_text.parse::<u16>().ok()?;
        return Some((host, port));
    }
    let port = default_port?;
    Some((host_port, port))
}

fn encode_dns_name(host: &str, dst: &mut [u8], cursor: &mut usize) -> bool {
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 {
            return false;
        }
        if !push_bytes(dst, cursor, &[label.len() as u8])
            || !push_bytes(dst, cursor, label.as_bytes())
        {
            return false;
        }
    }
    push_bytes(dst, cursor, &[0])
}

fn parse_dns_a_response(packet: &[u8], txid: u16) -> Option<[u8; 4]> {
    if packet.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([packet[0], packet[1]]);
    if id != txid {
        return None;
    }
    let flags = u16::from_be_bytes([packet[2], packet[3]]);
    if (flags & 0x8000) == 0 || (flags & 0x000f) != 0 {
        return None;
    }
    let qdcount = u16::from_be_bytes([packet[4], packet[5]]) as usize;
    let ancount = u16::from_be_bytes([packet[6], packet[7]]) as usize;
    let mut offset = 12usize;

    for _ in 0..qdcount {
        offset = skip_dns_name(packet, offset)?;
        if offset + 4 > packet.len() {
            return None;
        }
        offset += 4;
    }

    for _ in 0..ancount {
        offset = skip_dns_name(packet, offset)?;
        if offset + 10 > packet.len() {
            return None;
        }
        let rtype = u16::from_be_bytes([packet[offset], packet[offset + 1]]);
        let class = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]);
        let rdlen = u16::from_be_bytes([packet[offset + 8], packet[offset + 9]]) as usize;
        offset += 10;
        if offset + rdlen > packet.len() {
            return None;
        }
        if rtype == 1 && class == 1 && rdlen == 4 {
            return Some([
                packet[offset],
                packet[offset + 1],
                packet[offset + 2],
                packet[offset + 3],
            ]);
        }
        offset += rdlen;
    }
    None
}

fn skip_dns_name(packet: &[u8], mut offset: usize) -> Option<usize> {
    let mut steps = 0usize;
    while steps < 128 {
        if offset >= packet.len() {
            return None;
        }
        let len = packet[offset];
        if (len & 0xc0) == 0xc0 {
            if offset + 1 >= packet.len() {
                return None;
            }
            return Some(offset + 2);
        }
        if len == 0 {
            return Some(offset + 1);
        }
        let label_len = len as usize;
        if label_len > 63 || offset + 1 + label_len > packet.len() {
            return None;
        }
        offset += 1 + label_len;
        steps = steps.saturating_add(1);
    }
    None
}

fn push_bytes(dst: &mut [u8], cursor: &mut usize, src: &[u8]) -> bool {
    if dst.len().saturating_sub(*cursor) < src.len() {
        return false;
    }
    let end = *cursor + src.len();
    dst[*cursor..end].copy_from_slice(src);
    *cursor = end;
    true
}

fn parse_http_status_code(response: &[u8]) -> Option<u16> {
    let prefix = b"HTTP/";
    if response.len() < 12 || &response[..5] != prefix {
        return None;
    }
    let mut spaces_seen = 0usize;
    let mut i = 0usize;
    while i < response.len() {
        if response[i] == b' ' {
            spaces_seen = spaces_seen.saturating_add(1);
            if spaces_seen == 1 && i + 3 < response.len() {
                let a = response[i + 1];
                let b = response[i + 2];
                let c = response[i + 3];
                if a.is_ascii_digit() && b.is_ascii_digit() && c.is_ascii_digit() {
                    return Some(
                        ((a - b'0') as u16) * 100 + ((b - b'0') as u16) * 10 + (c - b'0') as u16,
                    );
                }
            }
        }
        if response[i] == b'\n' {
            break;
        }
        i = i.saturating_add(1);
    }
    None
}

fn with_net<R>(f: impl FnOnce(&NetState) -> R) -> R {
    let _guard = NET_LOCK.lock();
    // SAFETY: `NET_LOCK` serializes access to global network state.
    unsafe { f(&*NET_STATE.0.get()) }
}

fn with_net_mut<R>(f: impl FnOnce(&mut NetState) -> R) -> R {
    let _guard = NET_LOCK.lock();
    // SAFETY: `NET_LOCK` serializes mutable access to global network state.
    unsafe { f(&mut *NET_STATE.0.get()) }
}

fn queue_base_ptr(queue: u16) -> *mut u8 {
    // SAFETY: called while holding `NET_LOCK`.
    unsafe {
        if queue == RX_QUEUE_INDEX {
            (*RX_QUEUE_MEMORY.0.get()).bytes.as_mut_ptr()
        } else {
            (*TX_QUEUE_MEMORY.0.get()).bytes.as_mut_ptr()
        }
    }
}

unsafe fn queue_bytes_mut(queue: u16) -> &'static mut [u8; VRING_BYTES] {
    if queue == RX_QUEUE_INDEX {
        // SAFETY: caller guarantees synchronized access.
        unsafe { &mut (*RX_QUEUE_MEMORY.0.get()).bytes }
    } else {
        // SAFETY: caller guarantees synchronized access.
        unsafe { &mut (*TX_QUEUE_MEMORY.0.get()).bytes }
    }
}

unsafe fn queue_desc_ptr(queue: u16) -> *mut VirtqDesc {
    queue_base_ptr(queue) as *mut VirtqDesc
}

unsafe fn queue_avail_ptr(queue: u16) -> *mut VirtqAvail {
    // SAFETY: pointer arithmetic stays within queue backing allocation.
    unsafe { queue_base_ptr(queue).add(DESC_BYTES) as *mut VirtqAvail }
}

unsafe fn queue_used_ptr(queue: u16) -> *mut VirtqUsed {
    // SAFETY: pointer arithmetic stays within queue backing allocation.
    unsafe { queue_base_ptr(queue).add(USED_OFFSET) as *mut VirtqUsed }
}

fn rx_hdr_ptr() -> *mut VirtioNetHdr {
    // SAFETY: caller ensures synchronized access to RX buffer.
    unsafe { addr_of_mut!((*RX_BUFFER.0.get()).hdr) }
}

fn rx_frame_ptr() -> *mut u8 {
    // SAFETY: caller ensures synchronized access to RX buffer.
    unsafe { (*RX_BUFFER.0.get()).frame.as_mut_ptr() }
}

fn tx_hdr_ptr() -> *mut VirtioNetHdr {
    // SAFETY: caller ensures synchronized access to TX buffer.
    unsafe { addr_of_mut!((*TX_BUFFER.0.get()).hdr) }
}

fn tx_frame_ptr() -> *mut u8 {
    // SAFETY: caller ensures synchronized access to TX buffer.
    unsafe { (*TX_BUFFER.0.get()).frame.as_mut_ptr() }
}

fn find_virtio_net_pci() -> Option<PciLocation> {
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
                if device_id != VIRTIO_NET_TRANSITIONAL_ID && device_id != VIRTIO_NET_MODERN_ID {
                    continue;
                }

                let bar0 = pci_read_u32(bus as u8, device as u8, function as u8, 0x10);
                if (bar0 & 0x1) == 0 {
                    continue;
                }
                let io_base = (bar0 & !0x3) as u16;
                let command =
                    pci_read_u16(bus as u8, device as u8, function as u8, 0x04) | 0x1 | 0x4;
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
    // SAFETY: PCI config mechanism #1 on x86 uses 0xCF8/0xCFC ports.
    unsafe {
        port::outl(PCI_CONFIG_ADDR, address);
        port::inl(PCI_CONFIG_DATA)
    }
}

fn pci_write_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let address = pci_address(bus, device, function, offset);
    // SAFETY: PCI config mechanism #1 on x86 uses 0xCF8/0xCFC ports.
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
    let aligned = offset & !0x2;
    let mut dword = pci_read_u32(bus, device, function, aligned);
    let shift = ((offset & 0x2) * 8) as u32;
    dword &= !(0xFFFFu32 << shift);
    dword |= (value as u32) << shift;
    pci_write_u32(bus, device, function, aligned, dword);
}

fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        sum = sum.wrapping_add(u16::from_be_bytes([chunk[0], chunk[1]]) as u32);
    }
    if let Some(last) = chunks.remainder().first() {
        sum = sum.wrapping_add((*last as u32) << 8);
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn tcp_checksum(src_ip: [u8; 4], dst_ip: [u8; 4], segment: &[u8]) -> u16 {
    let mut sum = 0u32;
    sum = sum.wrapping_add(u16::from_be_bytes([src_ip[0], src_ip[1]]) as u32);
    sum = sum.wrapping_add(u16::from_be_bytes([src_ip[2], src_ip[3]]) as u32);
    sum = sum.wrapping_add(u16::from_be_bytes([dst_ip[0], dst_ip[1]]) as u32);
    sum = sum.wrapping_add(u16::from_be_bytes([dst_ip[2], dst_ip[3]]) as u32);
    sum = sum.wrapping_add(IP_PROTO_TCP as u32);
    sum = sum.wrapping_add(segment.len() as u32);

    let mut chunks = segment.chunks_exact(2);
    for chunk in &mut chunks {
        sum = sum.wrapping_add(u16::from_be_bytes([chunk[0], chunk[1]]) as u32);
    }
    if let Some(last) = chunks.remainder().first() {
        sum = sum.wrapping_add((*last as u32) << 8);
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
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
