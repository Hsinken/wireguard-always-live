# Security

WG Always Live runs a service with high privileges (LocalSystem on Windows,
root on Ubuntu) that can start and stop VPN tunnels. Reports of anything
that could let someone misuse it are welcome.

## What counts

For example:

- A way for an account not on the installation's access list to use the
  watchdog, or for a listed account to do more than it may: register a
  tunnel it may not, or turn automatic recovery on for one.
- A way to make WGAL run a command, read or write a file, or act on a
  service or interface that is not its own.
- The Bot Token, a WireGuard configuration or a key reaching a log, an
  export, an answer or an error message.
- A way for `wgal-mcp` to act while MCP is off, or to change the Telegram
  settings while agents may not.
- An install script that changes something outside WGAL's own files and
  services.

## How to report

There is no private reporting channel yet: GitHub's private vulnerability
reporting is not turned on for this repository.

Until there is one:

1. Open an issue that says only that you have a security report and asks
   how to send it. Put no details of the problem in it.
2. The maintainer will answer there with a private way to send it.

Never put any of these in an issue, a pull request or a report: a WireGuard
configuration, a private key, a Bot Token, a Chat ID, or a log that has not
been through WGAL's own redacted export.

