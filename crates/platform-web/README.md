# platform-web

Original MIT Rust website-session transports for Reddit and Discord. The local
operator owns the browser; the VPS performs bounded requests without a browser
or JavaScript engine. No passwords or mobile OAuth client credentials are used.

Reddit provides the signed-in homepage, posts and bounded comment context.
Discord provides DM/server/channel selection and message history, without a
Gateway connection. Common social/media tools own account-scoped references,
pagination and lazy image/attachment downloads. Writes are unavailable.

Browser login requires a positive authenticated-page probe followed by an
independent Runtime identity request before saving. Cookie presence alone is
insufficient. The browser remains open until Runtime verification finishes.
Sessions stay in private Rust/control-pipe storage, outside model context.

These adapters are experimental; synthetic tests do not establish current
real-account compatibility. Reddit may reject requests from a VPS. Discord
restricts normal-account automation under its [self-bot policy](https://support.discord.com/hc/en-us/articles/115002192352-Automated-User-Accounts-Self-Bots);
website-session reading is distinct from its official bot/OAuth APIs.

Protocol facts: Reddit's website JSON listings and own-profile response;
Discord's [resource schemas](https://docs.discord.com/developers/resources/user)
and website requests. No third-party implementation source is vendored here.
