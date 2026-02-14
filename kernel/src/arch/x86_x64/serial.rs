use spin::Mutex;
use uart_16550::SerialPort;

static SERIAL1: Mutex<Option<SerialPort>> = Mutex::new(None);

pub fn init() {
    let mut lock = SERIAL1.lock();
    let mut port = unsafe { SerialPort::new(0x3F8) }; // COM1
    port.init();
    *lock = Some(port);
}

pub fn log(s: &str) {
    use core::fmt::Write;
    if let Some(ref mut port) = *SERIAL1.lock() {
        let _ = port.write_str(s);
    }
}

pub fn logln<T: core::fmt::Display>(msg: T) {
    use core::fmt::Write;
    if let Some(ref mut port) = *SERIAL1.lock() {
        let _ = writeln!(port, "{msg}");
    }
}
