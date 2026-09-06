# Verified Platform Notes

This file contains only dated facts backed by an explicit live verification record or a dated executed benchmark. It is intentionally narrower than design and implementation documentation.

## Evidence rule

A fact belongs here only when the repository records the environment, date, method, and observed result. Compilation, source inspection, or vendor documentation alone does not establish runtime behavior. Re-verify when an invalidation condition applies.

## DocumentDB local vector and text behavior

**Verified:** 2026-08-23 against `ghcr.io/documentdb/documentdb/documentdb-local:latest`, gateway port 10260, using `mongosh` inside the container.

Observed:

- `createIndexes` accepted a `contentVector: "cosmosSearch"` index with `kind: "vector-ivf"`, `numLists: 100`, `similarity: "COS"`, and `dimensions: 1024`, returning `ok: 1`.
- A legacy text index on `text` was accepted and stored as `_fts`/`_ftsx` with text-index version 2.
- Vector retrieval worked with `$search.cosmosSearch` as the first aggregation stage and `$meta: "searchScore"` projection.
- Lexical retrieval worked with `$text`, `$meta: "textScore"`, and sorting by that metadata score.
- The tested local engine did not provide Atlas-style `$vectorSearch` or native `$rankFusion` for this path; vector and lexical queries therefore ran separately and were RRF-fused in Rust.

**Invalidated by:** changing the DocumentDB image/tag or build, moving to a different Mongo-compatible product/service, changing index dimensions/kind/options, adopting a different search operator, or upgrading the engine in a way that changes supported syntax or stage ordering. Re-run index creation and representative queries before relying on the result.

**Scope caution:** this proves the tested local container behavior. It does not prove identical behavior for every Azure-hosted Cosmos DB offering or later image.

## SQL Server/tiberius value decoding

**Verified:** 2026-08-23 against a live `mcr.microsoft.com/mssql/server` container. The opt-in Rust test is `connectors::mssql::tests::decodes_tricky_sql_server_types` with `RAG_MSSQL_LIVE=1`.

| SQL Server value | Observed tiberius decode | Stored JSON representation |
| --- | --- | --- |
| `INT`, `BIGINT`, `SMALLINT` | signed integer types | number |
| `TINYINT` | `u8` | number |
| `DECIMAL`, `NUMERIC` | `tiberius::numeric::Numeric` | string to retain exact precision |
| `MONEY` | `f64` | number |
| `REAL` | `f32` | number |
| `FLOAT` | `f64` | number |
| `BIT` | `bool` | boolean |
| text/NVARCHAR | `&str` | string |
| `DATETIME2`, `DATE` | chrono date/time types | string |
| `UNIQUEIDENTIFIER` | `uuid::Uuid` | 36-character string |

The verified decoder-order constraint is: primitive integers, `u8`, `f64`, `f32`, `Numeric`, then boolean/string fallbacks. In particular, moving `Numeric` before floating-point attempts changes the observed handling of `MONEY`/float types.

**Invalidated by:** changing tiberius version/features, SQL Server version/image, TDS protocol features, decode attempt order, JSON conversion policy, or adding/removing tested SQL types. Re-run the opt-in live test after such changes.

## UUID v7 persistence choice

**Verified:** 2026-08-23 by source update and successful Rust compilation, not by a comparative storage benchmark.

The server mints UUIDv7 strings for user, source, and ingestion-job identifiers. Record identifiers remain deterministic composite strings. The only proven claim is the implemented identifier format and compilation; no repository benchmark quantifies B-tree improvement.

**Invalidated by:** changing UUID crate features/API, reverting minting sites, adding a new UUID-minting site with a different policy, or changing the underlying identifier/index representation.

## Foundry Local execution-provider process boundary

**Observed:** 2026-08-23 after upgrading Foundry Local from 0.8.119 to 0.10.3.

- Running `foundry model list` downloaded/registered OpenVINO for the CLI's service instance.
- After server restart, the server's `GET /hardware` still reported its own discovered providers as unregistered.
- Source/API investigation established that this application embeds a Foundry core in-process; CLI registration did not register providers into that core.
- The implemented remedy calls `download_and_register_eps(None)` against the server's own manager and offers an administrator retry route.
- CPU is treated as the built-in fallback even when it is absent from `discover_eps()` output.

**Invalidated by:** Foundry Local/SDK upgrades, changing from the embedded manager to an external service, altered provider-registration semantics, cache relocation, or changing initialization order. Re-test with `GET /hardware`, provider registration, and an actual model load/generation on each targeted execution provider.

**Not proven by this record:** that OpenVINO, WebGPU, CUDA, QNN, Vitis, or any NPU/GPU path works on arbitrary target hardware. OS device discovery and provider registration report different facts; only an actual model execution validates usability.

## Synthetic Auto benchmark

**Executed:** 2026-09-01/02 on the repository's stated server build, synthetic PostgreSQL seed, DocumentDB, Foundry Local, and the app Auto UI path.

Recorded result:

- A01–A10: 10/10 strict passes, 100% coverage, 11 timed requests including the A10 follow-up, no timeout.
- End-to-end p50: 0.51 seconds; recorded p95 approximately 4.6 seconds; maximum 4.62 seconds for the final passing measurements.
- A02 and A03 list precision, recall, and F1 were each 1.00.
- The final A10 follow-up resolved the patient to Jane Chebet in 299 ms. A prior run had taken 73,056 ms and returned the wrong surname; that failure remains regression evidence.
- All final A-series passing answers used deterministic SQL, so this run did not validate the local model-planner fallback or semantic RAG quality.

Additional dated entries report S06 and S07 deterministic grouped-count joins passing on 2026-09-02. S08 and S09 were not run in the recorded matrix. The semantic S01 case was recorded as failed, with a low rerank score and correct refusal caused by an upstream retrieval/enrichment gap at that time.

**Invalidated by:** changing deterministic SQL matching, router logic/models, schema metadata/FK discovery, source seed, ingestion projection, conversation memory, bridge/UI rendering, database engine/version, model/EP, or benchmark questions/oracles. Re-run the relevant cases and retain the old result rather than overwriting failure history.

## Explicitly unverified platform claims

The repository does not currently contain dated proof for:

- production Android build/install and physical-device behavior;
- Android release signing, upgrade, rollback, hardware-back, or secure token storage;
- TLS termination/certificate pinning in a target deployment;
- DocumentDB CA validation with invalid-certificate acceptance removed;
- HNSW creation/query behavior on the deployed engine;
- eight-hour soak or 10/25/50-client capacity;
- larger-than-RAM ingestion and bounded peak RSS;
- complete crash-stage ingestion resume across every failure point;
- local RAGAs/judged generation scores; or
- all supported physical GPU/NPU execution providers.

These belong in a dated result only after execution, not in this file as inferred capability.

## Source plans consolidated

- [02 — Live Verification & Connector Decode Findings](../old/02-verification-and-decode-findings.md): primary dated DocumentDB, SQL Server, and UUID record.
- [05 — Accelerator Detection](../old/05-accelerator-detection.md): dated Foundry CLI-versus-embedded-core observation and implemented registration response.
- [Older decode and verification notes](../old/docs/decode-and-verification-notes.md): consolidated here with the overbroad “identical across local and Azure” wording narrowed to the environment actually tested.
- [`eval/AUTO_BENCHMARK.md`](../../eval/AUTO_BENCHMARK.md) and [`eval/QUESTIONS.md`](../../eval/QUESTIONS.md): dated executed synthetic benchmark evidence and explicit pass/fail/not-run states.
