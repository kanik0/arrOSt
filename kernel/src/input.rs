// kernel/src/input.rs: cross-arch input bridge (virtio-input polled runtime).
#[derive(Clone, Copy)]
pub struct InputInitReport {
    pub backend: &'static str,
    pub keyboard_ready: bool,
    pub mouse_ready: bool,
    pub keyboard_io_base: u16,
    pub mouse_io_base: u16,
}

mod virtio {
    use crate::arch::port;
    use crate::keyboard::{self, KeyCode, KeyEvent};
    use crate::mem;
    use crate::mouse::{self, MouseEvent};
    use core::cell::UnsafeCell;
    use core::hint::spin_loop;
    use core::mem::size_of;
    use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
    use core::sync::atomic::{AtomicBool, Ordering, fence};

    use super::InputInitReport;

    const VIRTIO_PCI_HOST_FEATURES: u16 = 0x00;
    const VIRTIO_PCI_GUEST_FEATURES: u16 = 0x04;
    const VIRTIO_PCI_QUEUE_PFN: u16 = 0x08;
    const VIRTIO_PCI_QUEUE_NUM: u16 = 0x0C;
    const VIRTIO_PCI_QUEUE_SEL: u16 = 0x0E;
    const VIRTIO_PCI_QUEUE_NOTIFY: u16 = 0x10;
    const VIRTIO_PCI_STATUS: u16 = 0x12;

    const VIRTIO_STATUS_ACK: u8 = 1;
    const VIRTIO_STATUS_DRIVER: u8 = 2;
    const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
    const VIRTIO_STATUS_FAILED: u8 = 128;

    const VIRTQ_DESC_F_WRITE: u16 = 2;

    const MAX_QUEUE_SIZE: u16 = 64;
    const MAX_QUEUE_SIZE_USIZE: usize = MAX_QUEUE_SIZE as usize;
    const VRING_ALIGN: usize = 4096;

    const EV_SYN: u16 = 0;
    const EV_KEY: u16 = 1;
    const EV_REL: u16 = 2;

    const KEY_ESC: u16 = 1;
    const KEY_1: u16 = 2;
    const KEY_2: u16 = 3;
    const KEY_3: u16 = 4;
    const KEY_4: u16 = 5;
    const KEY_5: u16 = 6;
    const KEY_6: u16 = 7;
    const KEY_7: u16 = 8;
    const KEY_8: u16 = 9;
    const KEY_9: u16 = 10;
    const KEY_0: u16 = 11;
    const KEY_MINUS: u16 = 12;
    const KEY_EQUAL: u16 = 13;
    const KEY_BACKSPACE: u16 = 14;
    const KEY_TAB: u16 = 15;
    const KEY_Q: u16 = 16;
    const KEY_W: u16 = 17;
    const KEY_E: u16 = 18;
    const KEY_R: u16 = 19;
    const KEY_T: u16 = 20;
    const KEY_Y: u16 = 21;
    const KEY_U: u16 = 22;
    const KEY_I: u16 = 23;
    const KEY_O: u16 = 24;
    const KEY_P: u16 = 25;
    const KEY_LEFTBRACE: u16 = 26;
    const KEY_RIGHTBRACE: u16 = 27;
    const KEY_ENTER: u16 = 28;
    const KEY_A: u16 = 30;
    const KEY_S: u16 = 31;
    const KEY_D: u16 = 32;
    const KEY_F: u16 = 33;
    const KEY_G: u16 = 34;
    const KEY_H: u16 = 35;
    const KEY_J: u16 = 36;
    const KEY_K: u16 = 37;
    const KEY_L: u16 = 38;
    const KEY_SEMICOLON: u16 = 39;
    const KEY_APOSTROPHE: u16 = 40;
    const KEY_GRAVE: u16 = 41;
    const KEY_LEFTSHIFT: u16 = 42;
    const KEY_BACKSLASH: u16 = 43;
    const KEY_Z: u16 = 44;
    const KEY_X: u16 = 45;
    const KEY_C: u16 = 46;
    const KEY_V: u16 = 47;
    const KEY_B: u16 = 48;
    const KEY_N: u16 = 49;
    const KEY_M: u16 = 50;
    const KEY_COMMA: u16 = 51;
    const KEY_DOT: u16 = 52;
    const KEY_SLASH: u16 = 53;
    const KEY_RIGHTSHIFT: u16 = 54;
    const KEY_SPACE: u16 = 57;
    const KEY_UP: u16 = 103;
    const KEY_LEFT: u16 = 105;
    const KEY_RIGHT: u16 = 106;
    const KEY_DOWN: u16 = 108;

    const REL_X: u16 = 0;
    const REL_Y: u16 = 1;

    const BTN_LEFT: u16 = 0x110;
    const BTN_RIGHT: u16 = 0x111;
    const BTN_MIDDLE: u16 = 0x112;

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

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VirtioInputEvent {
        event_type: u16,
        code: u16,
        value: i32,
    }

    const INPUT_EVENT_ZERO: VirtioInputEvent = VirtioInputEvent {
        event_type: 0,
        code: 0,
        value: 0,
    };

    const DESC_BYTES: usize = size_of::<VirtqDesc>() * MAX_QUEUE_SIZE_USIZE;
    const AVAIL_BYTES: usize = size_of::<VirtqAvail>();
    const USED_OFFSET: usize = align_up(DESC_BYTES + AVAIL_BYTES, VRING_ALIGN);
    const VRING_BYTES: usize = USED_OFFSET + size_of::<VirtqUsed>();

    #[repr(C, align(4096))]
    struct QueueMemory {
        bytes: [u8; VRING_BYTES],
    }

    #[repr(C, align(16))]
    struct EventBufferMemory {
        events: [VirtioInputEvent; MAX_QUEUE_SIZE_USIZE],
    }

    struct QueueMemoryCell(UnsafeCell<QueueMemory>);
    struct EventBufferCell(UnsafeCell<EventBufferMemory>);

    // SAFETY: access is serialized via `INPUT_LOCK`.
    unsafe impl Sync for QueueMemoryCell {}
    // SAFETY: access is serialized via `INPUT_LOCK`.
    unsafe impl Sync for EventBufferCell {}

    static KBD_QUEUE_MEMORY: QueueMemoryCell = QueueMemoryCell(UnsafeCell::new(QueueMemory {
        bytes: [0; VRING_BYTES],
    }));
    static KBD_EVENT_MEMORY: EventBufferCell =
        EventBufferCell(UnsafeCell::new(EventBufferMemory {
            events: [INPUT_EVENT_ZERO; MAX_QUEUE_SIZE_USIZE],
        }));

    static MOUSE_QUEUE_MEMORY: QueueMemoryCell = QueueMemoryCell(UnsafeCell::new(QueueMemory {
        bytes: [0; VRING_BYTES],
    }));
    static MOUSE_EVENT_MEMORY: EventBufferCell =
        EventBufferCell(UnsafeCell::new(EventBufferMemory {
            events: [INPUT_EVENT_ZERO; MAX_QUEUE_SIZE_USIZE],
        }));

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

    #[derive(Clone, Copy)]
    struct DeviceQueueState {
        io_base: u16,
        queue_size: u16,
        last_used_idx: u16,
        avail_idx: u16,
        ready: bool,
    }

    impl DeviceQueueState {
        const fn new() -> Self {
            Self {
                io_base: 0,
                queue_size: 0,
                last_used_idx: 0,
                avail_idx: 0,
                ready: false,
            }
        }
    }

    #[derive(Clone, Copy)]
    struct KeyboardState {
        device: DeviceQueueState,
        shift_left: bool,
        shift_right: bool,
    }

    impl KeyboardState {
        const fn new() -> Self {
            Self {
                device: DeviceQueueState::new(),
                shift_left: false,
                shift_right: false,
            }
        }

        fn shift_active(self) -> bool {
            self.shift_left || self.shift_right
        }
    }

    #[derive(Clone, Copy)]
    struct MouseState {
        device: DeviceQueueState,
        left_button: bool,
        right_button: bool,
        middle_button: bool,
        accum_dx: i32,
        accum_dy: i32,
        dirty: bool,
    }

    impl MouseState {
        const fn new() -> Self {
            Self {
                device: DeviceQueueState::new(),
                left_button: false,
                right_button: false,
                middle_button: false,
                accum_dx: 0,
                accum_dy: 0,
                dirty: false,
            }
        }
    }

    struct InputState {
        initialized: bool,
        keyboard: KeyboardState,
        mouse: MouseState,
        dropped_events: u64,
    }

    impl InputState {
        const fn new() -> Self {
            Self {
                initialized: false,
                keyboard: KeyboardState::new(),
                mouse: MouseState::new(),
                dropped_events: 0,
            }
        }

        fn report(&self) -> InputInitReport {
            InputInitReport {
                backend: if self.keyboard.device.ready || self.mouse.device.ready {
                    "virtio-input-polled"
                } else {
                    "none"
                },
                keyboard_ready: self.keyboard.device.ready,
                mouse_ready: self.mouse.device.ready,
                keyboard_io_base: self.keyboard.device.io_base,
                mouse_io_base: self.mouse.device.io_base,
            }
        }

        fn init(&mut self) -> InputInitReport {
            if self.initialized {
                return self.report();
            }

            self.initialized = true;
            self.dropped_events = 0;
            self.keyboard = KeyboardState::new();
            self.mouse = MouseState::new();

            if let Some(io_base) = port::virtio_input_keyboard_io_base() {
                let _ = init_device_queue(
                    io_base,
                    &KBD_QUEUE_MEMORY,
                    &KBD_EVENT_MEMORY,
                    &mut self.keyboard.device,
                );
            }

            if let Some(io_base) = port::virtio_input_mouse_io_base() {
                if init_device_queue(
                    io_base,
                    &MOUSE_QUEUE_MEMORY,
                    &MOUSE_EVENT_MEMORY,
                    &mut self.mouse.device,
                ) {
                    mouse::set_virtual_backend_ready();
                }
            }

            self.report()
        }

        fn poll(&mut self) {
            self.poll_keyboard();
            self.poll_mouse();
        }

        fn poll_keyboard(&mut self) {
            if !self.keyboard.device.ready {
                return;
            }

            while let Some((head, event)) = poll_one_event(
                &mut self.keyboard.device,
                &KBD_QUEUE_MEMORY,
                &KBD_EVENT_MEMORY,
            ) {
                self.handle_keyboard_event(event);
                requeue_descriptor(&mut self.keyboard.device, &KBD_QUEUE_MEMORY, head);
            }
        }

        fn poll_mouse(&mut self) {
            if !self.mouse.device.ready {
                return;
            }

            while let Some((head, event)) = poll_one_event(
                &mut self.mouse.device,
                &MOUSE_QUEUE_MEMORY,
                &MOUSE_EVENT_MEMORY,
            ) {
                self.handle_mouse_event(event);
                requeue_descriptor(&mut self.mouse.device, &MOUSE_QUEUE_MEMORY, head);
            }
        }

        fn handle_keyboard_event(&mut self, event: VirtioInputEvent) {
            if event.event_type != EV_KEY {
                return;
            }

            let pressed = event.value != 0;
            match event.code {
                KEY_LEFTSHIFT => {
                    self.keyboard.shift_left = pressed;
                    return;
                }
                KEY_RIGHTSHIFT => {
                    self.keyboard.shift_right = pressed;
                    return;
                }
                KEY_UP => {
                    keyboard::inject_key_event(KeyEvent {
                        code: KeyCode::ArrowUp,
                        pressed,
                    });
                    return;
                }
                KEY_DOWN => {
                    keyboard::inject_key_event(KeyEvent {
                        code: KeyCode::ArrowDown,
                        pressed,
                    });
                    return;
                }
                KEY_LEFT => {
                    keyboard::inject_key_event(KeyEvent {
                        code: KeyCode::ArrowLeft,
                        pressed,
                    });
                    return;
                }
                KEY_RIGHT => {
                    keyboard::inject_key_event(KeyEvent {
                        code: KeyCode::ArrowRight,
                        pressed,
                    });
                    return;
                }
                _ => {}
            }

            let Some(base) = map_linux_key_base(event.code) else {
                return;
            };

            keyboard::inject_key_event(KeyEvent {
                code: KeyCode::Byte(base),
                pressed,
            });

            if pressed {
                keyboard::inject_byte(apply_shift(base, self.keyboard.shift_active()));
            }
        }

        fn handle_mouse_event(&mut self, event: VirtioInputEvent) {
            match event.event_type {
                EV_KEY => {
                    let pressed = event.value != 0;
                    match event.code {
                        BTN_LEFT => self.mouse.left_button = pressed,
                        BTN_RIGHT => self.mouse.right_button = pressed,
                        BTN_MIDDLE => self.mouse.middle_button = pressed,
                        _ => return,
                    }
                    self.mouse.dirty = true;
                }
                EV_REL => {
                    match event.code {
                        REL_X => {
                            self.mouse.accum_dx = self.mouse.accum_dx.saturating_add(event.value);
                        }
                        REL_Y => {
                            // Convert Linux-style +Y down to the PS/2 convention used by gfx.
                            self.mouse.accum_dy = self.mouse.accum_dy.saturating_sub(event.value);
                        }
                        _ => return,
                    }
                    self.mouse.dirty = true;
                }
                EV_SYN => {
                    if !self.mouse.dirty {
                        return;
                    }
                    self.mouse.dirty = false;
                    let dx = clamp_i32_to_i16(self.mouse.accum_dx);
                    let dy = clamp_i32_to_i16(self.mouse.accum_dy);
                    self.mouse.accum_dx = 0;
                    self.mouse.accum_dy = 0;
                    mouse::inject_event(MouseEvent {
                        dx,
                        dy,
                        left_button: self.mouse.left_button,
                        right_button: self.mouse.right_button,
                        middle_button: self.mouse.middle_button,
                    });
                }
                _ => {}
            }
        }
    }

    struct InputCell(UnsafeCell<InputState>);

    // SAFETY: input state access is serialized by `INPUT_LOCK`.
    unsafe impl Sync for InputCell {}

    static INPUT_LOCK: SpinLock = SpinLock::new();
    static INPUT_STATE: InputCell = InputCell(UnsafeCell::new(InputState::new()));

    pub fn init() -> InputInitReport {
        with_state_mut(InputState::init)
    }

    pub fn poll() {
        with_state_mut(InputState::poll);
    }

    pub fn ready() -> bool {
        with_state_mut(|state| {
            state.initialized && (state.keyboard.device.ready || state.mouse.device.ready)
        })
    }

    fn with_state_mut<R>(f: impl FnOnce(&mut InputState) -> R) -> R {
        let _guard = INPUT_LOCK.lock();
        // SAFETY: `INPUT_LOCK` ensures exclusive mutable access.
        unsafe { f(&mut *INPUT_STATE.0.get()) }
    }

    fn map_linux_key_base(code: u16) -> Option<u8> {
        let byte = match code {
            KEY_ESC => 0x1b,
            KEY_1 => b'1',
            KEY_2 => b'2',
            KEY_3 => b'3',
            KEY_4 => b'4',
            KEY_5 => b'5',
            KEY_6 => b'6',
            KEY_7 => b'7',
            KEY_8 => b'8',
            KEY_9 => b'9',
            KEY_0 => b'0',
            KEY_MINUS => b'-',
            KEY_EQUAL => b'=',
            KEY_BACKSPACE => 0x08,
            KEY_TAB => b'\t',
            KEY_Q => b'q',
            KEY_W => b'w',
            KEY_E => b'e',
            KEY_R => b'r',
            KEY_T => b't',
            KEY_Y => b'y',
            KEY_U => b'u',
            KEY_I => b'i',
            KEY_O => b'o',
            KEY_P => b'p',
            KEY_LEFTBRACE => b'[',
            KEY_RIGHTBRACE => b']',
            KEY_ENTER => b'\n',
            KEY_A => b'a',
            KEY_S => b's',
            KEY_D => b'd',
            KEY_F => b'f',
            KEY_G => b'g',
            KEY_H => b'h',
            KEY_J => b'j',
            KEY_K => b'k',
            KEY_L => b'l',
            KEY_SEMICOLON => b';',
            KEY_APOSTROPHE => b'\'',
            KEY_GRAVE => b'`',
            KEY_BACKSLASH => b'\\',
            KEY_Z => b'z',
            KEY_X => b'x',
            KEY_C => b'c',
            KEY_V => b'v',
            KEY_B => b'b',
            KEY_N => b'n',
            KEY_M => b'm',
            KEY_COMMA => b',',
            KEY_DOT => b'.',
            KEY_SLASH => b'/',
            KEY_SPACE => b' ',
            _ => return None,
        };
        Some(byte)
    }

    fn apply_shift(byte: u8, shift: bool) -> u8 {
        if !shift {
            return byte;
        }
        match byte {
            b'a'..=b'z' => byte - 32,
            b'1' => b'!',
            b'2' => b'@',
            b'3' => b'#',
            b'4' => b'$',
            b'5' => b'%',
            b'6' => b'^',
            b'7' => b'&',
            b'8' => b'*',
            b'9' => b'(',
            b'0' => b')',
            b'-' => b'_',
            b'=' => b'+',
            b'[' => b'{',
            b']' => b'}',
            b';' => b':',
            b'\'' => b'"',
            b'`' => b'~',
            b'\\' => b'|',
            b',' => b'<',
            b'.' => b'>',
            b'/' => b'?',
            _ => byte,
        }
    }

    fn clamp_i32_to_i16(value: i32) -> i16 {
        if value > i16::MAX as i32 {
            i16::MAX
        } else if value < i16::MIN as i32 {
            i16::MIN
        } else {
            value as i16
        }
    }

    fn init_device_queue(
        io_base: u16,
        queue_memory: &QueueMemoryCell,
        event_memory: &EventBufferCell,
        state: &mut DeviceQueueState,
    ) -> bool {
        state.io_base = io_base;
        state.queue_size = 0;
        state.last_used_idx = 0;
        state.avail_idx = 0;
        state.ready = false;

        virtio_write_u8(io_base, VIRTIO_PCI_STATUS, 0);
        virtio_write_u8(io_base, VIRTIO_PCI_STATUS, VIRTIO_STATUS_ACK);
        virtio_write_u8(
            io_base,
            VIRTIO_PCI_STATUS,
            VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER,
        );

        let host_features = virtio_read_u32(io_base, VIRTIO_PCI_HOST_FEATURES);
        virtio_write_u32(io_base, VIRTIO_PCI_GUEST_FEATURES, host_features);

        virtio_write_u16(io_base, VIRTIO_PCI_QUEUE_SEL, 0);
        let queue_max = virtio_read_u16(io_base, VIRTIO_PCI_QUEUE_NUM);
        if queue_max == 0 {
            virtio_write_u8(io_base, VIRTIO_PCI_STATUS, VIRTIO_STATUS_FAILED);
            return false;
        }

        let queue_size = queue_max.min(MAX_QUEUE_SIZE);
        virtio_write_u16(io_base, VIRTIO_PCI_QUEUE_NUM, queue_size);
        let queue_size = virtio_read_u16(io_base, VIRTIO_PCI_QUEUE_NUM).min(MAX_QUEUE_SIZE);
        if queue_size == 0 {
            virtio_write_u8(io_base, VIRTIO_PCI_STATUS, VIRTIO_STATUS_FAILED);
            return false;
        }

        // SAFETY: input lock serializes queue memory access.
        unsafe {
            (*queue_memory.0.get()).bytes.fill(0);
            (*event_memory.0.get()).events.fill(INPUT_EVENT_ZERO);
        }

        let queue_virt = queue_memory_base(queue_memory) as usize;
        let Some(queue_phys) = mem::virt_to_phys(queue_virt) else {
            virtio_write_u8(io_base, VIRTIO_PCI_STATUS, VIRTIO_STATUS_FAILED);
            return false;
        };
        if !queue_phys.is_multiple_of(VRING_ALIGN as u64) {
            virtio_write_u8(io_base, VIRTIO_PCI_STATUS, VIRTIO_STATUS_FAILED);
            return false;
        }

        virtio_write_u32(io_base, VIRTIO_PCI_QUEUE_PFN, (queue_phys >> 12) as u32);
        if virtio_read_u32(io_base, VIRTIO_PCI_QUEUE_PFN) == 0 {
            virtio_write_u8(io_base, VIRTIO_PCI_STATUS, VIRTIO_STATUS_FAILED);
            return false;
        }

        // SAFETY: queue pointers are within queue allocation and lock is held.
        unsafe {
            let desc = queue_desc_ptr(queue_memory);
            let avail = queue_avail_ptr(queue_memory);
            for index in 0..usize::from(queue_size) {
                let event_ptr = addr_of_mut!((*event_memory.0.get()).events[index]) as usize;
                let Some(event_phys) = mem::virt_to_phys(event_ptr) else {
                    virtio_write_u8(io_base, VIRTIO_PCI_STATUS, VIRTIO_STATUS_FAILED);
                    return false;
                };

                write_volatile(
                    desc.add(index),
                    VirtqDesc {
                        addr: event_phys,
                        len: size_of::<VirtioInputEvent>() as u32,
                        flags: VIRTQ_DESC_F_WRITE,
                        next: 0,
                    },
                );
                write_volatile(addr_of_mut!((*avail).ring[index]), index as u16);
            }

            write_volatile(addr_of_mut!((*avail).idx), queue_size);
        }

        state.queue_size = queue_size;
        state.avail_idx = queue_size;
        state.last_used_idx = 0;

        fence(Ordering::SeqCst);
        virtio_write_u16(io_base, VIRTIO_PCI_QUEUE_NOTIFY, 0);

        virtio_write_u8(
            io_base,
            VIRTIO_PCI_STATUS,
            VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
        );
        state.ready = true;
        true
    }

    fn poll_one_event(
        state: &mut DeviceQueueState,
        queue_memory: &QueueMemoryCell,
        event_memory: &EventBufferCell,
    ) -> Option<(u16, VirtioInputEvent)> {
        if !state.ready || state.queue_size == 0 {
            return None;
        }

        // SAFETY: queue pointers are valid while lock is held.
        unsafe {
            let used = queue_used_ptr(queue_memory);
            let used_idx = read_volatile(addr_of!((*used).idx));
            if used_idx == state.last_used_idx {
                return None;
            }

            let used_slot = (state.last_used_idx % state.queue_size) as usize;
            let used_elem = read_volatile(addr_of!((*used).ring[used_slot]));
            state.last_used_idx = state.last_used_idx.wrapping_add(1);

            let head = used_elem.id as usize;
            if head >= usize::from(state.queue_size) {
                return None;
            }

            let event = (*event_memory.0.get()).events[head];
            Some((head as u16, event))
        }
    }

    fn requeue_descriptor(state: &mut DeviceQueueState, queue_memory: &QueueMemoryCell, head: u16) {
        if !state.ready || state.queue_size == 0 {
            return;
        }

        // SAFETY: queue pointers are valid while lock is held.
        unsafe {
            let avail = queue_avail_ptr(queue_memory);
            let slot = (state.avail_idx % state.queue_size) as usize;
            write_volatile(addr_of_mut!((*avail).ring[slot]), head);
            state.avail_idx = state.avail_idx.wrapping_add(1);
            fence(Ordering::Release);
            write_volatile(addr_of_mut!((*avail).idx), state.avail_idx);
        }

        virtio_write_u16(state.io_base, VIRTIO_PCI_QUEUE_NOTIFY, 0);
    }

    fn queue_memory_base(queue_memory: &QueueMemoryCell) -> *mut u8 {
        // SAFETY: caller holds input lock.
        unsafe { (*queue_memory.0.get()).bytes.as_mut_ptr() }
    }

    unsafe fn queue_desc_ptr(queue_memory: &QueueMemoryCell) -> *mut VirtqDesc {
        queue_memory_base(queue_memory) as *mut VirtqDesc
    }

    unsafe fn queue_avail_ptr(queue_memory: &QueueMemoryCell) -> *mut VirtqAvail {
        // SAFETY: pointer arithmetic stays within the queue allocation.
        unsafe { queue_memory_base(queue_memory).add(DESC_BYTES) as *mut VirtqAvail }
    }

    unsafe fn queue_used_ptr(queue_memory: &QueueMemoryCell) -> *mut VirtqUsed {
        // SAFETY: pointer arithmetic stays within the queue allocation.
        unsafe { queue_memory_base(queue_memory).add(USED_OFFSET) as *mut VirtqUsed }
    }

    fn virtio_read_u8(io_base: u16, offset: u16) -> u8 {
        // SAFETY: `io_base + offset` is a validated virtio legacy I/O range.
        unsafe { port::inb(io_base + offset) }
    }

    fn virtio_write_u8(io_base: u16, offset: u16, value: u8) {
        // SAFETY: `io_base + offset` is a validated virtio legacy I/O range.
        unsafe { port::outb(io_base + offset, value) }
    }

    fn virtio_read_u16(io_base: u16, offset: u16) -> u16 {
        // SAFETY: `io_base + offset` is a validated virtio legacy I/O range.
        unsafe { port::inw(io_base + offset) }
    }

    fn virtio_write_u16(io_base: u16, offset: u16, value: u16) {
        // SAFETY: `io_base + offset` is a validated virtio legacy I/O range.
        unsafe { port::outw(io_base + offset, value) }
    }

    fn virtio_read_u32(io_base: u16, offset: u16) -> u32 {
        // SAFETY: `io_base + offset` is a validated virtio legacy I/O range.
        unsafe { port::inl(io_base + offset) }
    }

    fn virtio_write_u32(io_base: u16, offset: u16, value: u32) {
        // SAFETY: `io_base + offset` is a validated virtio legacy I/O range.
        unsafe { port::outl(io_base + offset, value) }
    }
}

pub fn init() -> InputInitReport {
    virtio::init()
}

pub fn poll() {
    virtio::poll()
}

pub fn virtio_ready() -> bool {
    virtio::ready()
}
