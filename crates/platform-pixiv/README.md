# platform-pixiv

Original MIT Rust implementation of a small read-only Pixiv website adapter.
The operator hands off a web session; this crate does not include a browser,
execute scripts, collect passwords or borrow official mobile OAuth credentials.

Account verification reads the own-account projection from authenticated HTML:
legacy `meta-global-data.userData`, or Next.js `isLoggedIn` plus the session actor
in `gaUserData.userId`. The latter fetches only that already verified actor’s
public display name and checks the returned ID. Runtime binds the confirmed
actor before saving. Public creator metadata and PHPSESSID prefixes alone are
not authentication evidence.

Runtime exposes safe-mode recommendation snapshots, artwork metadata and lazy
regular/original image references through existing social/media tools. Images
stream from the fixed Pixiv CDN with no account Cookie. Comments, writes,
homepage pagination and ugoira animation decoding are unavailable.

Synthetic validation covers cookie/ID parsing, identity projection and secret
redaction. Real account acceptance is recorded separately; public source alone
does not establish current website compatibility.
