# Deterministic evaluation

The judge-free smoke runner measures router accuracy and retrieval hit-rate/MRR against the synthetic PostgreSQL seed. It sends data only to the local on-premises server.

## Prerequisites

1. Start DocumentDB and the seeded development PostgreSQL source.
2. Start the server with Foundry Local available.
3. Register and ingest the seeded PostgreSQL source, including `medication_catalog` and `allergen_catalog`.

## Run

```powershell
$env:ONPREM_EVAL_USERNAME = "admin"
$env:ONPREM_EVAL_PASSWORD = "password"
node eval\run.mjs --suite full --smoke
```

Available suites are `router`, `retrieval`, and `full`. Available retrieval configurations are `full`, `naive`, `no-rerank`, and `fast`:

```powershell
node eval\run.mjs --suite retrieval --config no-rerank
```

Override the server with `ONPREM_EVAL_URL` or `--base-url`. The runner exits nonzero when router accuracy is below 95%, retrieval hit-rate@6 is below 100%, or MRR is below 0.5. Timestamped JSON reports are written to `eval\reports\` and are ignored by Git.

Fixtures contain only deterministic synthetic seed references. Do not add production questions, record text, credentials, or PHI.
