#![no_std]
#![no_main]

// user/init/src/bin/ping.rs
// Ring-3 ping: one ICMP echo to an IPv4 host via SYS_PING.
// Usage: ping <ip>

use arrostd::user_entry;

#[path = "common/mod.rs"]
mod support;

user_entry!(main);

fn main(argc: usize, argv: *const *const u8) -> i32 {
    let args = support::args(argc, argv);
    if args.len() < 2 {
        return support::write_usage("ping", "usage: /bin/ping <host>");
    }
    let host = match args.get(1) {
        Some(h) => h,
        None => return support::write_usage("ping", "usage: /bin/ping <host>"),
    };
    let ip = match parse_ipv4(host) {
        Some(ip) => ip,
        None => {
            support::write_text("ping: invalid host address\n");
            arrostd::runtime::exit(1);
        }
    };
    write_ping_header(ip);
    let rtt = arrostd::runtime::ping(ip);
    if rtt >= 0 {
        write_ping_reply(ip, rtt as u64);
        write_ping_stats(ip, true);
        arrostd::runtime::exit(0)
    } else {
        support::write_text("Request timeout for icmp_seq 1\n");
        write_ping_stats(ip, false);
        arrostd::runtime::exit(1)
    }
}

fn write_ping_header(ip: [u8; 4]) {
    support::write_text("PING ");
    write_ip(ip);
    support::write_text(": 56 data bytes\n");
}

fn write_ping_reply(ip: [u8; 4], rtt_ms: u64) {
    support::write_text("64 bytes from ");
    write_ip(ip);
    support::write_text(": icmp_seq=1 ttl=64 time=");
    write_u64(rtt_ms);
    support::write_text(" ms\n");
}

fn write_ping_stats(ip: [u8; 4], received: bool) {
    support::write_text("\n--- ");
    write_ip(ip);
    support::write_text(" ping statistics ---\n");
    if received {
        support::write_text("1 packets transmitted, 1 received, 0% packet loss\n");
    } else {
        support::write_text("1 packets transmitted, 0 received, 100% packet loss\n");
    }
}

fn write_ip(ip: [u8; 4]) {
    write_u64(ip[0] as u64);
    support::write_text(".");
    write_u64(ip[1] as u64);
    support::write_text(".");
    write_u64(ip[2] as u64);
    support::write_text(".");
    write_u64(ip[3] as u64);
}

fn write_u64(mut v: u64) {
    let mut digits = [0u8; 20];
    let mut len = 0usize;
    loop {
        digits[len] = b'0' + (v % 10) as u8;
        len += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let mut buf = [0u8; 20];
    for i in 0..len {
        buf[i] = digits[len - 1 - i];
    }
    let _ = arrostd::runtime::write_stdout(&buf[..len]);
}

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

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    arrostd::runtime::exit(1);
}
