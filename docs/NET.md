# Networking

ArrOSt provides a virtio-net based networking stack aimed at practical debugging and smoke-testable behavior.

## Backend

- Device backend: virtio-net (legacy PCI path)
- Environment: QEMU user-mode networking with optional host forwarding

## Protocol support (current)

- Ethernet framing
- ARP
- IPv4
- ICMP echo (ping)
- UDP send/receive path
- Minimal TCP path used by simple HTTP `curl` flow
- DHCP and DNS helper paths for runtime configuration/use

## Shell integration

- `net`
- `ping <a.b.c.d>`
- `udp send <a.b.c.d> <port> <text>`
- `udp last`
- `curl udp://<ip>:<port>/<payload>`
- `curl http://<host|ip>[:port]/<path>`

## Limits

- Not a full production TCP/IP stack.
- Focused on deterministic behavior inside QEMU.
- Limited socket/API surface via current syscall model.

## Relevant files

- `kernel/src/net/mod.rs`
- `kernel/src/proc/mod.rs`
- `kernel/src/shell.rs`
- `scripts/qemu.sh`
