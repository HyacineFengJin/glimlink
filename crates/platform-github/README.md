# platform-github

Small synchronous GitHub REST adapter for Agent Runtime. Uses existing ureq/rustls,
with no browser, JavaScript runtime, async executor, SDK daemon, or account probing.
API version: `2026-03-10`.

The adapter supports public reads, token-authenticated identity, repository and
issue/PR metadata, issue discussion and review comments, repository issue/PR pages,
notifications, PR changed-file pages, and repository contents. Writes include one explicit issue/PR discussion comment, issue creation, PR reviews with inline comments, inline replies, open/closed state, labels and assignees updates. Each SDK operation submits one fixed native request without replay. Runtime owns account selection,
opaque references, durable operations, and reconciliation. Tokens stay in headers
and are redacted from configuration Debug output and errors.

All requests are reconstructed under `https://api.github.com`. Redirects and
environment proxies are disabled. Paths are validated and encoded. Returned URLs
are untrusted data. Link URLs are never followed: a validated next-page number is
the only extracted value, including GitHub's canonical `/repositories/<id>/...`
form. Invalid links preserve the current page and report `pagination_warning` with
unknown `has_more`, rather than claiming the collection ended.

Decoded HTTP response bodies are limited to 1 MiB, including decompressed JSON.
Raw file bytes are separately limited to 1 MiB and encoded locally as Base64, so
the resulting Base64 value can exceed 1 MiB. `contents` returns directory entries
or file metadata, while `content_file` reads regular files using the raw media
type. If inline Base64 makes file metadata exceed the JSON limit, the adapter may
obtain matching file metadata from its bounded parent-directory listing. If that
listing is unavailable or too large, the read fails explicitly. Directories are
never returned as file bytes. Symlinks and submodules are not expanded by this
adapter. GitHub may itself resolve a symlink to a regular in-repository file.

Call `resolve_revision` once and retain the returned commit SHA in file references;
the method requests GitHub's SHA-only media type to avoid downloading commit diffs.
Both metadata and raw requests must use this frozen revision for stable reads.
Directory responses can contain at most 1000 entries and report possible
truncation at that boundary. PR file listings can contain at most 3000 files;
reaching the API cap reports an unknown tail. A missing diff patch is marked
unavailable, rather than represented as an empty diff.

Notifications require authentication and currently support a classic personal
access token with the `notifications` or `repo` scope; fine-grained tokens are not
supported by that GitHub endpoint. Reads do not request mark-as-read. The adapter
returns `X-Poll-Interval` when present but does not poll automatically.

No POST is retried. A network loss, malformed success, or ambiguous server failure
after submission is `WriteResultUnknown`. The caller must retain that operation
and reconcile it; it must not send a replacement blindly. A successful comment
response is checked for its exact repository, issue number, and body. A precise
`read_comment` is available for Runtime's read-back verification.

Official protocol references:

- [REST authentication](https://docs.github.com/en/rest/authentication/authenticating-to-the-rest-api)
- [Pagination](https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api)
- [Issue comments](https://docs.github.com/en/rest/issues/comments?apiVersion=2026-03-10)
- [Repository contents](https://docs.github.com/en/rest/repos/contents?apiVersion=2026-03-10)
- [Commit reference resolution](https://docs.github.com/en/rest/commits/commits?apiVersion=2026-03-10)
- [Pull requests and changed files](https://docs.github.com/en/rest/pulls/pulls?apiVersion=2026-03-10)
- [Notifications](https://docs.github.com/en/rest/activity/notifications?apiVersion=2026-03-10)


`CollaborationAction` uses fixed endpoints from GitHub's official REST documentation: [issues](https://docs.github.com/en/rest/issues/issues?apiVersion=2026-03-10), [reviews](https://docs.github.com/en/rest/pulls/reviews?apiVersion=2026-03-10), and [review comments](https://docs.github.com/en/rest/pulls/comments?apiVersion=2026-03-10). Inline reviews use actual file paths and a frozen PR head commit, not diff positions guessed by the model. The host accepts returned `file_ref` values, checks account/repository/revision, and verifies acknowledged inline comments against the exact review-scoped listing including author, review, commit and line coordinates. `reviews` provides a paged review summary listing.

The Runtime `github_action` tool owns exact target references and durable operations. Lost/malformed success responses remain unknown and are never resubmitted automatically. Close/reopen affects state only; there is no merge, push or delete action. All new collaboration tests are synthetic Docker tests; no live issue/review/state mutation was performed in this development pass.
