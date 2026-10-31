# Codex direct tool catalog

`codex-0.165.0-direct.json` is derived from OpenAI Codex's Apache-2.0 licensed
[`rust-v0.165.0` model catalog](https://github.com/openai/codex/blob/rust-v0.165.0/codex-rs/models-manager/models.json).
The upstream license and a local attribution notice are included. All 12 model entries
are preserved; the only semantic change is `tool_mode = "direct"` for each model.
Artifact SHA256: `12c95136595685d3fc7235e43aa0263c4c410304f5c681b4a36f7e01ee6ab279`.

Codex gives model metadata precedence over `features.code_mode`; therefore
disabling its code-mode host alone leaves some models trying an unavailable
JavaScript execution tool. Runtime uses the upstream direct function-call path
instead. It never downloads or starts `codex-code-mode-host`, whose separate
dependency graph includes V8. Subscription authentication, model slugs, backend
endpoints and permissions are unchanged.

The startup `model_catalog_json` setting replaces the complete model catalog;
it is not a partial overlay and does not merge subsequent remote metadata.
Keep the catalog paired with the pinned native CLI. Operators may supply an
explicit replacement catalog when upgrading. Model availability still depends
on the account and server; this file does not grant model access.
