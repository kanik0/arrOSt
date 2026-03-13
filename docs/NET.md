# Networking

ArrOSt provides a virtio-net based networking stack aimed at practical debugging and smoke-testable behavior.

## Backend

- Device backend: virtio-net (`virtio-pci` legacy-compatible queue path on `x86_64`, virtio-mmio legacy-compatible queue path on `aarch64`)
- Environment: QEMU user-mode networking with optional host forwarding

## Protocol support (current)

- Ethernet framing
- ARP
- IPv4
- ICMP echo (ping)
- UDP send/receive path
- TCP state machine (SYN_SENT → ESTABLISHED → FIN_WAIT_1 / CLOSE_WAIT) used by `curl http://...` and BSD socket syscalls
- DHCP and DNS helper paths for runtime configuration/use

## BSD Socket API (syscall ABI revision 5)

Ring-3 processes can open TCP sockets via the standard BSD socket interface:

| Syscall | Number | Description |
|---------|--------|-------------|
| `socket(AF_INET, SOCK_STREAM, IPPROTO_TCP)` | 6 | Create a TCP socket fd |
| `bind(fd, addr, addrlen)` | 47 | Bind socket to local port |
| `listen(fd, backlog)` | 48 | Mark socket as passive listener |
| `accept(fd, addr, addrlen)` | 49 | Accept incoming connection (blocking, 500-tick timeout) |
| `connect(fd, addr, len)` | 50 | Connect to remote host (blocking) |
| `send(fd, buf, len, flags)` | 51 | Send data on a connected socket |
| `recv(fd, buf, len, flags)` | 52 | Receive data from a connected socket |
| `close(fd)` | 16 | Close socket and send FIN |

The connection table supports up to `MAX_TCP_CONNS=4` concurrent TCP connections and `MAX_TCP_LISTENERS=4` passive listeners per kernel.
TCP congestion control (slow start, Reno-style CWND) and TIME_WAIT timer (≈4 s) are active.

## Shell integration

- `net` — show interface stats
- `ping <a.b.c.d>` — send ICMP echo request
- `traceroute <a.b.c.d>` / `/bin/traceroute` — ICMP TTL-probe hop-by-hop trace
- `host <name>` / `/bin/host` — DNS A-record lookup, `<name> has address <ip>` output
- `dig <name> [A]` / `/bin/dig` — verbose DNS lookup with QUESTION/ANSWER sections
- `udp send <a.b.c.d> <port> <text>` — send UDP datagram
- `udp last` — show last received UDP message
- `curl udp://<ip>:<port>/<payload>` — UDP curl
- `curl http://<host|ip>[:port]/<path>` — minimal HTTP GET via TCP
- `netstat` / `/bin/netstat` — show active TCP connections + stats
- `ifconfig` / `/bin/ifconfig` — show network interface configuration
- `route` / `/bin/route` — show kernel IP routing table
- `arp` / `/bin/arp` — show ARP cache
- `ss` / `/bin/ss` — show socket summary (alias for netstat)
- `nc <host> <port>` / `/bin/nc` — netcat stub (reports usage; interactive not supported)
- `ip [addr|link|route]` / `/bin/ip` — ip utility (delegates to ifconfig/route)

## /bin executables

The following network utility commands are available as virtual `/bin/*` executables
(listed by `ls /bin`, dispatched via the shell binary execution path):

- `/bin/netstat`
- `/bin/ifconfig`
- `/bin/route`
- `/bin/arp`
- `/bin/ss`
- `/bin/nc`
- `/bin/ip`
- `/bin/traceroute`
- `/bin/host`
- `/bin/dig`

## Smoke test

```bash
cargo xtask smoke-net --arch x86_64
cargo xtask smoke-net --arch aarch64
```

The `smoke-net` harness boots QEMU, waits for the shell, then verifies:
- `net` command outputs `net: backend=...`
- `netstat` outputs connection table header
- `ifconfig` shows `eth0:` line
- `route` shows routing table header
- `arp` shows ARP cache header
- `ss` shows socket summary header
- `ip addr` delegates to ifconfig
- `ls /bin` includes `/bin/netstat`, `/bin/ifconfig`, `/bin/ip`

## M19 status

M19 is complete. All planned networking features are delivered. Network utilities (`netstat`, `ifconfig`, `route`, `arp`, `ss`, `nc`, `ip`, `ping`, `traceroute`, `host`, `dig`) are kernel-side shell commands dispatched as `/bin/*` entries through the kernel-mediated spawn path.

## Limits

- TCP retransmission queue (RTO, Karn's algorithm, exponential backoff) is not implemented; this is not meaningful in QEMU's slirp user-mode networking where packet delivery is reliable.
- Maximum 4 concurrent TCP connections; maximum 4 passive listeners.
- `nc` interactive mode not supported; use `curl` for HTTP.
- `traceroute` in QEMU user-mode networking shows the gateway (10.0.2.2) at TTL=1 and `* * *` for subsequent hops, since slirp does not simulate intermediate routers.
- User-space ring-3 ELF binaries for network utilities depend on M22 (`execve`); kernel-mediated dispatch is the current path.
- Not a full production TCP/IP stack; focused on deterministic behavior inside QEMU.

## Relevant files

- `kernel/src/net/mod.rs` — network stack, TCP state machine, ifconfig/netstat/route/arp helpers
- `kernel/src/fs/fd.rs` — FdTarget::TcpSocket variant
- `kernel/src/fs/mod.rs` — BIN_EXEC_PATHS (network utilities)
- `kernel/src/proc/mod.rs` — socket/connect/send/recv syscall dispatch
- `kernel/src/shell.rs` — shell commands for network utilities
- `crates/arrostd/src/lib.rs` — SYS_CONNECT/SYS_SEND/SYS_RECV constants, TcpConnectReq
- `kernel/src/arch/aarch64/port.rs` — virtio-mmio shim
- `scripts/qemu.sh`
- `scripts/qemu-aarch64.sh`
