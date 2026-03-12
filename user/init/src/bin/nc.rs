#![no_std]
#![no_main]

// user/init/src/bin/nc.rs
// Ring-3 netcat: minimal TCP client / server stub.
//
// Client mode:  nc <host> <port>
//   Connects to <host>:<port> (IPv4 dotted-decimal), then relays:
//     TCP → stdout  and  stdin → TCP
//   until the remote side closes the connection or stdin returns EOF.
//
// Listen mode:  nc -l <port>
//   Requires bind/listen/accept (SYS_BIND/SYS_LISTEN/SYS_ACCEPT) which are
//   currently stubs returning ENOSYS.  A helpful message is printed instead.

use arrostd::syscall::TcpConnectReq;
use arrostd::user_entry;
use arrostd::{runtime, syscall::errno};

#[path = "common/mod.rs"]
mod support;

user_entry!(main);

/// RX / TX scratch buffers (static, single-threaded).
static mut RX_BUF: [u8; 512] = [0u8; 512];
static mut TX_BUF: [u8; 512] = [0u8; 512];

fn main(argc: usize, argv: *const *const u8) -> i32 {
    let args = support::args(argc, argv);

    // nc -l <port>  — listen mode not yet supported
    if args.get(1) == Some("-l") {
        support::write_text("nc: listen mode not yet available (requires bind/listen/accept)\n");
        return arrostd::runtime::exit(1);
    }

    // nc <host> <port>
    if args.len() < 3 {
        return support::write_usage("nc", "usage: /bin/nc <host> <port>");
    }

    let host = match args.get(1) {
        Some(h) => h,
        None => return support::write_usage("nc", "usage: /bin/nc <host> <port>"),
    };
    let port_str = match args.get(2) {
        Some(p) => p,
        None => return support::write_usage("nc", "usage: /bin/nc <host> <port>"),
    };

    let ip = match parse_ipv4(host) {
        Some(ip) => ip,
        None => {
            support::write_text("nc: invalid host address\n");
            return arrostd::runtime::exit(1);
        }
    };
    let port = match parse_u16(port_str) {
        Some(p) => p,
        None => {
            support::write_text("nc: invalid port number\n");
            return arrostd::runtime::exit(1);
        }
    };

    // Use an ephemeral source port in 49152-65535 range (IANA dynamic).
    let src_port: u16 = 49200;
    let req = TcpConnectReq::new(ip, port, src_port);
    let fd_rc = runtime::tcp_connect(&req);
    if fd_rc < 0 {
        support::write_text("nc: connect failed (");
        support::write_text(errno::name(fd_rc));
        support::write_text(")\n");
        return arrostd::runtime::exit(1);
    }
    let fd = fd_rc as u32;

    // Half-duplex relay loop:
    //   1. Drain any pending data arriving from the remote peer → stdout.
    //   2. Read one chunk from stdin → send to remote peer.
    // Exits when either side closes the connection.
    loop {
        // Receive from TCP → stdout (non-blocking semantics: 0 bytes = no data yet).
        // SAFETY: single-threaded userland.
        let rx = unsafe { &mut *core::ptr::addr_of_mut!(RX_BUF) };
        let n = runtime::tcp_recv(fd, rx);
        if n < 0 {
            break; // connection error or reset
        }
        if n > 0 {
            let _ = runtime::write_stdout(&rx[..n as usize]);
        }

        // Read from stdin → TCP.
        // SAFETY: single-threaded userland.
        let tx = unsafe { &mut *core::ptr::addr_of_mut!(TX_BUF) };
        let m = runtime::fread(0, tx);
        if m <= 0 {
            break; // EOF on stdin
        }
        let sent = runtime::tcp_send(fd, &tx[..m as usize]);
        if sent < 0 {
            break; // send error
        }
    }

    let _ = runtime::close(fd);
    arrostd::runtime::exit(0)
}

/// Parse a dotted-decimal IPv4 address (e.g. "10.0.2.4") into `[u8; 4]`.
fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut idx = 0usize;
    let mut cur: u16 = 0;
    let mut digit_seen = false;

    for byte in s.bytes() {
        match byte {
            b'0'..=b'9' => {
                cur = cur * 10 + (byte - b'0') as u16;
                if cur > 255 {
                    return None;
                }
                digit_seen = true;
            }
            b'.' => {
                if !digit_seen || idx >= 3 {
                    return None;
                }
                octets[idx] = cur as u8;
                idx += 1;
                cur = 0;
                digit_seen = false;
            }
            _ => return None,
        }
    }

    if !digit_seen || idx != 3 {
        return None;
    }
    octets[3] = cur as u8;
    Some(octets)
}

/// Parse a decimal string into a `u16`.  Returns `None` on overflow or
/// non-digit input.
fn parse_u16(s: &str) -> Option<u16> {
    if s.is_empty() {
        return None;
    }
    let mut value: u32 = 0;
    for byte in s.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + (byte - b'0') as u32;
        if value > u16::MAX as u32 {
            return None;
        }
    }
    Some(value as u16)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    arrostd::runtime::exit(1);
}
