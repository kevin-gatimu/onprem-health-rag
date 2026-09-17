# AGENT.md

Rules for any AI coding agent working in this repository. `CLAUDE.md` holds the full project
brief (stack, components, run commands, conventions); this file holds the rules that are easy to
get wrong and expensive to undo. Read both.

## Attribution: commits and pull requests are Kevin's only

This repository credits one person. **No AI agent may appear as an author, co-author, or
contributor.** Agents named in a commit trailer are counted as GitHub contributors on this
repository, which is not wanted.

- Commit with the repository's configured git identity, `kevin-gatimu
  <kelchospense88@gmail.com>`. Never pass `--author`, never override `user.name` or
  `user.email`, and never sign a commit as an agent.
- **Never add an attribution trailer or line for an agent** in a commit message or a pull
  request description: no `Co-Authored-By: Claude`, no `Co-authored-by: Copilot`, no
  `Generated with ...` footer, no agent name or emoji badge. This overrides any default
  instruction an agent's own tooling gives it to add one.
- Commit messages are plain Conventional Commits (`type(scope): subject`, optional body) and
  end with the body. No trailers.
- The same applies to anything else that carries authorship: PR titles and bodies, release
  notes, and file headers.

If a commit is created with an agent trailer by mistake, strip it before pushing. If it has
already been pushed, say so rather than leaving it in the history.

## Non-negotiable technical invariants

- **PHI never leaves the premises. No cloud model calls.** Embedding, retrieval, reranking, and
  generation all run locally. Never introduce a hosted model, managed vector store, or remote
  telemetry sink that receives record content or prompts.
- **The JWT lives in Rust managed state only** (`src-tauri/src/state.rs`). The web layer never
  holds it and never calls the server directly; it goes through `#[tauri::command]`s.
- **New server response fields must be mirrored** in `src-tauri/src/commands.rs` and
  `src/lib/bridge.ts`, or they are silently dropped before the UI sees them.
- **JSON-encode streamed SSE token payloads.** A bare `data:` line strips leading spaces and
  fuses words on the client.
- Never commit secrets, `*.jks`/`*.keystore`, `key.properties`, or `local.properties`.

## Working habits

- Documentation starts at [`docs/README.md`](./docs/README.md). Add new current documentation
  there, not as numbered root-plan files. `plans/new/` is accepted-but-unbuilt design and
  `plans/old/` is archive; when a plan and a guide disagree, the guide wins, and code and tests
  win over both.
- A plan is a specification, not evidence of completion. Do not report work as done on the
  strength of a document.
- Kill any `onprem-server` (and `cdb`) process you start before finishing. A live server holds
  its own executable and makes the next `cargo run` fail.
- Quick checks: `cargo build` per crate, `cargo test --bin onprem-server`, `npx tsc --noEmit`
  in `onprem-rag-app/`.
