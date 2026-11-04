# Account feeds for roaming

`social_discover({"action":"sources"})` lists saved accounts' feed entry points without contacting a
platform or exposing credentials. Its default source is `recommended`.
Use a returned `feed_ref` with `social_discover`; the reference restores the
platform, source and account. A saved session is checked when a network page is
read. Failed authentication never substitutes anonymous or popular content.

`social_discover({"action":"sources","source":"following"})` lists the separate following sources.
Recommendation and following references, pagination, deduplication and viewer
checks remain distinct. Change sources by selecting another feed, not by
changing an existing continuation. A continuation alone restores its owner.

| Platform | `recommended` | `following` |
| --- | --- | --- |
| Bilibili | Authenticated homepage recommendations | Following dynamics; explicitly promoted cards omitted |
| Tieba | Account's native personalized homepage | Real followed forums, rotating through their newest threads |
| X | For You | Following timeline |
| pixiv | Account homepage recommendation section; optional personalized tag topic | Followed authors' latest artwork |
| Reddit | Account's Best home feed | Subscription Hot feed: subscribed communities and platform-provided followed-profile content |
| Discord | Personalized Highlights notifications, not a home timeline | Unavailable; joined servers are not a followed-author feed |
| GitHub | Unavailable; repository activity is not a personalized recommendation algorithm | Received following/watched activity |

QQ remains a chat adapter. Weixin personal-account access remains paused.
Implementation and protocol tests do not imply that an unconfigured account
has passed real-account acceptance; per-account results are recorded in
[validation](validation.md).

## Reading

1. Call `social_discover({"action":"sources"})` and select a returned `feed_ref`.
2. Call `social_discover({"feed_ref":"…","limit":5})`.
3. Open returned content targets directly, or pass `continuation` unchanged to
   `social_discover`. Fetch images/video/audio only when needed.

Pixiv returns `available_topics` from that account's `recommendByTag` sections.
Pass one as `topic` on a new read of the same recommendation feed. This selects
a real personalized section, not a search or ranking. Its recommended ID order
is joined to the shared thumbnail pool; unrelated pool entries are excluded.
Recommendation batches can be paged locally and refreshed; this is explicitly
different from the platform's stable following-page sequence.

Tieba following first reads the account's real `like_forum` list, then fetches
at most one forum page per network batch. Cached tails are returned first and
later batches rotate forums. This is a followed-forum traversal, not a global
algorithmic ranking. Returned `forum_ref` values open with `social_open`; a
selected forum's continuation also uses `social_discover`. Pinned/featured flags,
the list cap and the followed-forum snapshot are explicit.

Discord only exposes known personalized notification types. Mentions, friend
requests and unknown recommendation types are not mixed in. Selecting a returned
highlight reads its exact message when identified, never an unrelated recent
message. No acknowledgement or read-state mutation is requested. GitHub retains
the received-events API's pagination, polling interval and limited history
window; event bodies/files are fetched only after selecting their targets.

## Protocol provenance

All new Runtime code is original project code under MIT. Platform names/assets
retain their owners and terms in the [provider asset manifest](../operator-desktop/frontend/brands/sources.json).
Public JavaScript is inspected as protocol data; none is executed by Runtime.

- [Reddit home-feed recommendations](https://support.reddithelp.com/hc/en-us/articles/4402284777364-What-are-home-feed-recommendations).
- [Discord Highlights and personalization](https://support.discord.com/hc/en-us/articles/5304469213079-Highlights-Notifications)
  and the [maintainer's fixed protocol revision](https://github.com/discord-userdoccers/discord-userdoccers/blob/f0f8a1fe3824ce31fee1848465c0d408d9980d30/pages/resources/notification-center.mdx).
- [GitHub received events](https://docs.github.com/en/rest/activity/events#list-events-received-by-the-authenticated-user).
- Pixiv and X use the website's current authenticated response schema and
  published query metadata; Tieba's native request-schema provenance is recorded
  in its [adapter README](../crates/platform-tieba/README.md).
