# platform-pixiv

Original MIT Rust implementation of a small read-only Pixiv website adapter.
The operator hands off a web session; this crate does not include a browser,
execute scripts, collect passwords or borrow official mobile OAuth credentials.

Account verification reads only the authenticated HTML `meta-global-data`
`userData` projection. Runtime binds the confirmed user to the selected alias
before privately saving the session. Public creator metadata and PHPSESSID
prefixes are not authentication evidence.

Runtime exposes safe-mode recommendation snapshots, artwork metadata and lazy
regular/original image references through existing social/media tools. Images
stream from the fixed Pixiv CDN with no account Cookie. Comments, writes,
homepage pagination and ugoira animation decoding are unavailable.

Synthetic validation covers cookie/ID parsing, identity projection and secret
redaction. Real account acceptance is recorded separately; public source alone
does not establish current website compatibility.
