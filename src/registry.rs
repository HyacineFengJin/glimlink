// SPDX-License-Identifier: MIT
use runtime_core::{Capability, CapabilityState as S, ModuleInfo};

fn capability(name: &str, state: S, detail: &str) -> Capability {
    Capability {
        name: name.into(),
        state,
        detail: detail.into(),
    }
}

pub fn modules() -> Vec<ModuleInfo> {
    vec![
        ModuleInfo {
            id: "platform-pixiv".into(), kind: "platform".into(),
            capabilities: vec![
                capability("web_login", S::Implemented, "Operator-installed browser hands off only the Pixiv web session; independent authenticated HTML identity verification and alias binding before persistence. Live account acceptance pending."),
                capability("read", S::Implemented, "Read-only safe-mode recommendations and artwork metadata; opaque targets and lazy regular/original image references use shared media reading. Homepage continuation and ugoira decoding are unavailable."),
            ],
        },
        ModuleInfo {
            id: "runtime-mind".into(),
            kind: "core".into(),
            capabilities: vec![
                capability(
                    "shared_identity",
                    S::Implemented,
                    "Operator persona plus agent-specific role injected at every managed model start; the same persona can span agents and conversations. External MCP clients read context under their own instruction hierarchy.",
                ),
                capability(
                    "shared_memory",
                    S::Implemented,
                    "Bounded private disk store with retrieval, revisions and provenance; typed people, explicit cross-platform identity claims, relationships, episodes and conversation recap. Shared across explicitly selected mind roots; no resident index.",
                ),
                capability(
                    "recent_dialogue",
                    S::Implemented,
                    "Portable local conversation history, 8 turns/16KiB per persona+conversation, separate from long-term memory and action receipts. Each managed task starts a fresh engine thread with current context.",
                ),
            ],
        },
        ModuleInfo {
            id: "runtime-core".into(),
            kind: "core".into(),
            capabilities: vec![
                capability(
                    "resource_store",
                    S::Implemented,
                    "Account-scoped disk resources, bounded stream writes and range reads; high-water cache maintenance between outer tasks, never during a composed media batch",
                ),
                capability(
                    "tool_router",
                    S::Implemented,
                    "Serial bounded tools; own-content comments use durable one-shot operation records",
                ),
                capability(
                    "ai_client_tasks",
                    S::Implemented,
                    "Default model tools open content, discuss, read creator feeds, notifications and publications, send to exact supported targets, and read/watch media. Runtime restores account-bound references, tracks pagination and fits real image attachments; legacy routes remain host-side.",
                ),
                capability(
                    "mcp_transport",
                    S::Implemented,
                    "Standard bounded MCP stdio: 2026-07-28 discovery/per-request metadata and 2025-11-25/2025-06-18 initialize compatibility, real image content, default task tools and the same durable outbox",
                ),
            ],
        },
        ModuleInfo {
            id: "runtime-jobs".into(),
            kind: "core".into(),
            capabilities: vec![
                capability(
                    "durable_queue",
                    S::Implemented,
                    "Bounded private queue, short transaction locks, one active lease, progress and operation IDs persisted before tools; interrupted jobs are never automatically replayed.",
                ),
                capability(
                    "operator_controls",
                    S::Implemented,
                    "Local enqueue/status/cancel/terminal GC and explicit recovery of only pre-model authentication failures; management remains available while the host owns Runtime.",
                ),
            ],
        },
        ModuleInfo {
            id: "agent-host".into(),
            kind: "host".into(),
            capabilities: vec![
                capability(
                    "on_demand_execution",
                    S::Implemented,
                    "Persistent operator agent profiles freeze engine/model/tool/default-account policy when jobs are enqueued; a serial source-to-job loop selects Codex or Antigravity per job; one model process runs only for queued work. Native 0.165.0 subscription direct-tool and real-image turns passed controlled acceptance; no native audio/video input.",
                ),
                capability(
                    "event_sources",
                    S::Implemented,
                    "Explicit watches with commit after enqueue. QQ incoming-only baseline; notifications use durable bounded catch-up, defer unknown prefixes until an old frontier is proven, and halt with gaps when proof/budget fails. No watches or automatic platform actions by default; Weixin stays paused.",
                ),
            ],
        },
        ModuleInfo {
            id: "platform-bilibili".into(),
            kind: "platform".into(),
            capabilities: vec![
                capability(
                    "public_video_read",
                    S::Implemented,
                    "Video metadata, WBI, low-resolution AVC/AAC selection and streamed download",
                ),
                capability(
                    "discovery_and_discussion",
                    S::Implemented,
                    "Separate signed-in homepage recommendations, public popular list and search; refresh-batch provenance/deduplication, snapshot statistics, cursor comments and paged replies. Media stays lazy and account-scoped.",
                ),
                capability(
                    "creator_content",
                    S::Implemented,
                    "Resolve a video's uploader inside Runtime and read public video posts and dynamics, with lazy media and independent partial failures",
                ),
                capability(
                    "login",
                    S::Implemented,
                    "Human-only web QR login, private saved session and identity binding; real-account acceptance recorded separately",
                ),
                capability(
                    "write",
                    S::Implemented,
                    "Public video comments/replies with separate authenticated actor and uploader; favorites/folder contents, coin, timed native protobuf danmaku and dynamic read/create/like/reply with internal image upload. Durable one-shot attempts; new writes require separate real-account acceptance",
                ),
                capability(
                    "notifications",
                    S::Implemented,
                    "Bounded reply, mention and received-like pages plus unread counts; no explicit acknowledgement API; implicit server effects unverified",
                ),
                capability(
                    "own_publications",
                    S::Implemented,
                    "Published space-video list for the verified current account; not creator drafts or media understanding",
                ),
            ],
        },
        ModuleInfo {
            id: "platform-qq".into(),
            kind: "platform".into(),
            capabilities: vec![
                capability(
                    "text_chat",
                    S::Implemented,
                    "Rust QQ library with saved-session login; friends/groups/member directories, observed chats, native server history, quote/mention/face, lazy received image/GIF/voice/video/file and one-call rich sends. Native upload and Highway stay inside the operation. New writes have synthetic protocol acceptance; real-account acceptance is separate; no implicit QR fallback.",
                ),
                capability(
                    "audio",
                    S::Implemented,
                    "Native QQ voice resource/send protocol, lazy download and ordinary supported audio -> Rust AMR-NB conversion. AMR-NB/WB decode implemented; Skype/Tencent SILK-v3 decode is implemented in Rust with pinned reference fixtures. Whisper speech text is distinct from general sound understanding.",
                ),
            ],
        },
        ModuleInfo {
            id: "platform-github".into(),
            kind: "application".into(),
            capabilities: vec![
                capability(
                    "repository_and_collaboration",
                    S::Implemented,
                    "Dedicated GitHub task tools for repositories, issues/pull requests, comments, notifications, and bounded lazy files/changes. Fixed REST transport; public reads need no login.",
                ),
                capability(
                    "account_and_comment",
                    S::Implemented,
                    "Management-only token login; conversation comments, Issue creation, exact-head PR reviews/line replies, state/labels/assignees with durable execution and exact readback. New authenticated writes have not been live accepted; no merge or push.",
                ),
            ],
        },
        ModuleInfo {
            id:"platform-x".into(), kind:"platform".into(),
            capabilities:vec![
                capability("web_session",S::Implemented,"Private import and identity-bound reuse of the user's own X browser session; no developer API key or automatic fresh login."),
                capability("read",S::Implemented,"Shared social task tools for profile/post opening, author posts and own publications, with opaque account-bound targets, bounded cursors and on-demand media. Real-account acceptance is recorded separately."),
                capability("write",S::Unavailable,"This adapter currently implements reads only; posting, replies, likes, follows and direct messages have not been enabled."),
            ],
        },
        ModuleInfo {
            id: "platform-weixin".into(),
            kind: "platform".into(),
            capabilities: vec![
                capability(
                    "personal_chat",
                    if cfg!(feature = "weixin-live") {
                        S::Implemented
                    } else {
                        S::Unavailable
                    },
                    if cfg!(feature = "weixin-live") {
                        "WebWx interfaces enabled explicitly; real account login and chat have not passed acceptance"
                    } else {
                        "Personal-account testing paused; WebWx client excluded from the default build, no login or automatic retry"
                    },
                ),
                capability(
                    "audio",
                    S::Planned,
                    "WebWx voice retrieval remains feature-gated and account testing paused; shared Rust SILK/AMR decoding and speech transcription exist, but the Weixin account/media chain has not been accepted.",
                ),
            ],
        },
        ModuleInfo {
            id: "platform-tieba".into(),
            kind: "platform".into(),
            capabilities: vec![
                capability(
                    "threads",
                    S::Implemented,
                    "Read-only protobuf thread pages, replies and opaque media assets; public data verified",
                ),
                capability(
                    "login",
                    S::Implemented,
                    "Human-only Passport QR flow, private BDUSS session and identity verification; real-account acceptance recorded separately",
                ),
                capability(
                    "notifications",
                    S::Implemented,
                    "Bounded native reply, mention and received-like feeds; read-state effects unknown; no explicit clear-unread request",
                ),
                capability(
                    "own_publications",
                    S::Implemented,
                    "Read-only self-identity check then distinct own-topic/own-reply feeds; parent threads are context and unknown pagination stays unknown",
                ),
                capability(
                    "write",
                    S::Implemented,
                    "One-call new threads, exact floor/child replies and image upload; search/forum browse and native continuations. Durable operation replay. New writes are implemented with protocol fixtures; prior live unknown send remains unverified; likes/deletion unavailable.",
                ),
            ],
        },
        ModuleInfo {
            id: "runtime-content".into(),
            kind: "content".into(),
            capabilities: vec![
                capability(
                    "html",
                    S::Implemented,
                    "Bounded tokenizer projection; original HTML, scripts and styles selectable by reference; no JS execution or DOM",
                ),
                capability(
                    "json",
                    S::Implemented,
                    "Bounded JSON Pointer selection with explicit truncation",
                ),
            ],
        },
        ModuleInfo {
            id: "runtime-media".into(),
            kind: "content".into(),
            capabilities: vec![
                capability(
                    "image",
                    S::Implemented,
                    "Bounded JPEG/PNG/WebP/GIF still-image preparation",
                ),
                capability(
                    "audio",
                    S::Implemented,
                    "AAC/MP3/PCM/Vorbis, pure-Rust AMR-NB/WB and standalone Skype/Tencent SILK-v3 to bounded WAV; pure-Rust AMR-NB voice encoding. Optional pinned quantized Whisper tiny supplies window-aligned speech text on demand; no environment-sound model.",
                ),
                capability(
                    "video",
                    S::Implemented,
                    "MP4/H264 sampling, PTS and coverage; persistent watch start/next/seek/status/finish with automatic aligned subtitle and optional speech preparation. Sampling does not prove full observation.",
                ),
            ],
        },
        ModuleInfo {
            id: "engine-codex".into(),
            kind: "engine".into(),
            capabilities: vec![
                capability(
                    "subscription_bridge",
                    S::Implemented,
                    "Native Codex app-server stdio; dedicated subscription login, real direct tools and image-result input accepted on pinned 0.165.0/gpt-6.1-sol; separate V8 code-mode host stays disabled",
                ),
                capability(
                    "native_audio_video",
                    S::Unavailable,
                    "Unsupported by this subscription input route",
                ),
            ],
        },
        ModuleInfo {
            id: "engine-antigravity".into(),
            kind: "engine".into(),
            capabilities: vec![
                capability(
                    "subscription_bridge",
                    S::Implemented,
                    "Native agy 1.4.11 ACP with dedicated Google login cache. Real subscription text and shared persona/memory tools accepted; authentication stays a separate operator flow with no API-key fallback.",
                ),
                capability(
                    "runtime_tools",
                    S::Implemented,
                    "Native Rust MCP proxy forwards to the same locked Runtime; shared persist-before-dispatch callback, allowlist, job context and action evidence. Built-in tools and default customizations disabled.",
                ),
                capability(
                    "tool_result_images",
                    S::Implemented,
                    "Real native subscription acceptance: media_read MCP image bytes correctly recognized. Direct ACP image input remains unavailable.",
                ),
                capability(
                    "native_media_input",
                    S::Unavailable,
                    "Direct image/audio/video ACP inputs are rejected; prepared media and model understanding are separate capabilities.",
                ),
            ],
        },
    ]
}
