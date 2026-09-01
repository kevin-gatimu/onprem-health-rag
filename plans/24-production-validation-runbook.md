# Production validation runbook

Run only with synthetic records on an isolated on-premises staging host. Record the commit, hardware, model variants, corpus size, server configuration, and start/end RSS with every result.

## Preflight

1. Start DocumentDB, Foundry Local, the seeded development source, and the server.
2. Ingest the deterministic seed and run `node eval\run.mjs --suite full --smoke`.
3. Save the evaluation JSON and `GET /metrics/summary` response.

## Load ladder

Run the same warm query at increasing concurrency:

```powershell
$env:ONPREM_LOAD_CONCURRENCY=10; $env:ONPREM_LOAD_REQUESTS=100; node perf\load.mjs
$env:ONPREM_LOAD_CONCURRENCY=25; $env:ONPREM_LOAD_REQUESTS=250; node perf\load.mjs
$env:ONPREM_LOAD_CONCURRENCY=50; $env:ONPREM_LOAD_REQUESTS=500; node perf\load.mjs
```

A valid overload result has bounded RSS, no task/queue growth after traffic stops, clear `429` responses at capacity, and no unexpected 5xx responses. Repeat with one client aborting requests to verify permits are released.

## Ingestion recovery

1. Complete generation A and confirm its records are searchable.
2. Start replacement generation B and stop the server during source read, embedding, writing, and cutover in separate trials.
3. Restart after every trial. The prior completed generation must remain searchable and the abandoned job must be visible and resumable or safely restartable.
4. Ingest a corpus larger than RAM and sample RSS each minute. RSS must plateau rather than grow with total rows.

## Eight-hour soak

Hold 10, 25, then 50 active clients while periodically ingesting synthetic data. Capture RSS, CPU/GPU/NPU utilization, request percentiles, errors, admission rejections, cache hit rates, and retrieval metrics. Pass only when RSS, queue depth, and task count remain bounded and deterministic retrieval gates do not regress.

## Failure drills

- Stop and restart DocumentDB during retrieval and ingestion.
- Make Foundry Local unavailable before and during generation.
- Make a source database unreachable mid-ingest.
- Exhaust disk space only on a disposable staging volume.
- Rotate JWT and credential keys using the deployment procedure.

Expected behavior is a sanitized error, no PHI in logs, released admission permits, preserved last-known-good indexed data, and successful recovery after the dependency returns.
