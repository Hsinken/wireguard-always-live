# Changelog

Every release of WG Always Live, newest first. Since 4.2.1 the window, the
watchdog, `wgal-mcp` and `wgal` share one version number. A version may be
released for some platforms only; each entry says which.

Each release archive carries a `RELEASE.txt` with the commit it was built
from and the SHA-256 of every file in it. The Releases page gives the
SHA-256 of each archive.

## 4.2.2 (2026-11-20)

- **Released for**: macOS only, built for the owner's Mac to test; not on
  the Releases page. Windows and Ubuntu did not change and stay at 4.2.1.
- **Fixed on macOS**: on macOS 27.2.1 the watchdog could not list the
  tunnels (`manager_query_failed`). That macOS writes the service type in
  `scutil --nc list` as `[VPN:com.wireguard.macos]`, with a colon; the
  parser accepted only `[VPN/com.wireguard.macos]`, and one line it could
  not read rejected the whole list. Both are read now.
- Every compatibility minimum stays 4.2.1.

## 4.2.1 (2026-11-19)

The first public version. Earlier versions were developed before the source
was published and were never released here.

- **Released for**: no platform on the Releases page yet. An Ubuntu build,
  made before the source was published, runs on a test server. Release
  archives for Windows 11, macOS and Ubuntu will be listed here when they
  are built from this repository.
- **What it has**: everything the [README](README.md) describes, including
  the window, the background watchdog, the command line `wgal`, the MCP
  server `wgal-mcp`, Telegram notifications and 13 languages.
- **One version** for the whole set, and every compatibility minimum
  (`MIN_GUI_VERSION`, `MIN_USABLE_GUI_VERSION`, `MIN_WATCHDOG_VERSION`,
  `MIN_USABLE_WATCHDOG_VERSION`) set to 4.2.1.
- **On Ubuntu**, probes bound to the tunnel's interface count, so the light
  can turn green and automatic recovery can be turned on. This was proven
  on a device; see [docs/testing.md](docs/testing.md) for what was and was
  not.
- **Messages** name the computer by its host name when no device name is
  set, and use the system's time zone as it is when each message is
  written.
