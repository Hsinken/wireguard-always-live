# Windows adapter fixtures

Anonymized stand-ins for what the Service Control Manager and the IP Helper
API report on a Windows host with WireGuard for Windows, read by the fakes in
`crates/wgal-platform/src/windows`. Service and tunnel names, interface
indexes and LUIDs are invented; addresses are from the RFC 5737
documentation ranges (and, for IPv6, RFC 3849 `2001:db8::/32` or
link-local), apart from the loopback adapter. None of this was
captured from a real host.

`host-three-tunnels.json` holds:

- `services`: every service name the SCM would list, with its
  `dwCurrentState` (1 stopped, 2 start pending, 4 running). It includes
  `WireGuardManager`, other services and names that are not valid tunnel
  services, which the adapter must skip.
- `adapters`: what `GetAdaptersAddresses` would report. Each may list
  `ipv6` addresses too (absent means none); the fake leaves them out of the
  IPv4 listing the probe path reads, and keeps them in the listing of both
  families the offline check reads. The `lab` tunnel is stopped, so it has
  no adapter.
- `routes`: what `GetBestRoute2` would answer for each destination, with no
  interface or source constraint.
- `default_routes`: the `0.0.0.0/0` and `::/0` rows `GetIpForwardTable2`
  would return for both families: one, through the `Ethernet` adapter's
  IPv4 gateway.
- `neighbors`: the neighbor (ARP or IPv6 neighbor discovery) entries:
  whether each is static, as `GetIpNetEntry2` would report, and whether
  the neighbor answers when `ResolveIpNetEntry2` asks for it. The gateway
  answers, so the host is online until a test takes its network away.
