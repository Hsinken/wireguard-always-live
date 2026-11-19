# Linux adapter fixtures

Anonymized stand-ins for the output of `systemctl`, `ip -j`, `ping` and
`wg show` on Ubuntu, read by `crates/wgal-platform/src/linux`. Interface
names, interface indexes and public keys are invented; addresses are from
the RFC 5737 documentation ranges. None of this was captured from a real
host. `*.stderr.txt` files hold what the tool prints on standard error.
