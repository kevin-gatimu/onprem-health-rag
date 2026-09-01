# Concurrent chat orchestration

## Goal

Keep navigation responsive while chat and agent responses stream, allow independent conversations to run concurrently, and serialize follow-up questions within one conversation unless the user explicitly sends one immediately.

## Design

1. Replace each feature's single pending run with a run registry keyed by `run_id` and queues keyed by conversation ID.
2. Keep the active chat and active agent conversation in Zustand so route changes do not discard the user's location or live response.
3. Route every bridge event by `run_id`; batch token events per run so concurrent streams cannot mix tokens.
4. Move request lifetime management into app-level orchestrators. Components submit work but do not own it, so unmounting a screen cannot cancel UI tracking or queue progression.
5. A normal send starts immediately when its conversation is idle and queues when that same conversation is busy. Different conversations and chat/agent features run concurrently.
6. While a conversation is busy, expose both **Queue** and **Send now**. Send now bypasses that conversation's queue; it may not include an unfinished answer in server-loaded history.
7. Render every active run and queued question in its owning conversation. Refresh persisted messages after each completion and atomically remove successful optimistic runs.
8. Keep navigation and new-chat controls enabled. Prevent destructive rename/delete actions only for conversations that currently have active work.

## ChatGPT-style UX findings

Public streaming guidance from OpenAI, Anthropic, MDN, React, TanStack Query, and Zustand supports a thread-first model: server history belongs in TanStack Query, while per-thread drafts, queues, and run state belong in Zustand. Streams are independent typed event sequences and must be routed by `run_id`, not by the currently visible conversation.

Implemented product behaviors:

- Switching routes, agent kinds, or conversations does not interrupt active streams.
- Each conversation keeps an in-memory draft; drafts are cleared on logout and are deliberately not written to browser storage because they may contain PHI.
- Active work is visible in the conversation list and destructive actions are disabled only for that conversation.
- Queued prompts can be removed or sent immediately.
- Failed and stopped runs retain partial output and can be retried.
- Each active run can be stopped independently through a Tauri cancellation command that drops its server stream.
- Stream updates use polite live regions and do not force-scroll users who have scrolled away from the bottom.

Editing historical prompts and branching conversations are deferred until the server has an explicit branch/message-parent data model; destructive mutation would make audit history ambiguous.

## Validation

Completed:

- React type-check and production build.
- Tauri `cargo check`.
- Server test suite: 59 passed.
- Focused code review of token routing, queue advancement, conversation creation, cancellation, retry, and navigation persistence.
- Stop-before-registration races are handled by a bounded, time-limited cancellation registry; logout aborts all active bridge runs.
