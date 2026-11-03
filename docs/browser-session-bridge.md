<!-- SPDX-License-Identifier: MIT -->
# Ordinary-browser session handoff

Pixiv login uses the system's ordinary browser and the small Glimlink Session
Bridge extension. It reuses the account already signed in there; it does not
start a debugging browser, read a browser database or ask for a password.

In this development build the operator writes the original MIT extension to
`$XDG_DATA_HOME/glimlink/browser-extension` (or `~/.local/share/glimlink/browser-extension`).
Load that directory once through Chrome/Edge's extension management page. This
is an unpacked development extension, not a published Web Store package.
The browser's site-access permission is requested when connecting, then revoked
after the attempt. There is no background worker, telemetry or credential storage.
If the browser interrupts cleanup, site access can also be removed in the
browser's extension manager; a failed automatic revocation is shown explicitly.

Click Pixiv's browser-login button in Operator. Its private local page shows the
selected Runtime and account alias, then opens the extension's connection page.
Keep the ordinary browser's authenticated Pixiv homepage open and connect there.
Only `PHPSESSID` is handed to local Rust, then through the existing local/SSH
control connection. Cookies, passwords and HTML are absent from model input,
Tauri events and logs. Runtime must independently verify identity before saving;
the extension's acknowledgement is not a successful Runtime login.

The local listener binds only `127.0.0.1`, with an unpredictable per-flow path,
strict Host/Origin checks, bounded requests and expiry/cancellation. A submitted
session ends the listener. No VPS port or browser is introduced. Normal browser
tabs are never closed by this flow. Other platforms retain their existing login
transports while ordinary-browser integration is expanded and accepted.

Cloudflare [does not support automated browsers for production challenges](https://developers.cloudflare.com/cloudflare-challenges/reference/supported-browsers/).
This bridge leaves human login in the ordinary browser. It does not solve
challenges, alter browser fingerprints or transfer clearance cookies to another
network. A working local login does not establish that a VPS's Rust requests will
be accepted; a denied VPS connection remains an explicit failed verification.

Synthetic Rust, extension-API and real-browser pairing tests are separate from
real-account acceptance. No source-only test establishes current provider access.
