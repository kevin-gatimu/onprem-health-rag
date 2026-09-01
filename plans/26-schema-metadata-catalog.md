# Versioned schema metadata catalog

## Purpose

Keep operational-database structure available locally so routing and NL-to-SQL can select tables, joins, and safe lookup columns without rediscovering the schema or asking a model to infer relationships on every request.

## Captured metadata

Each table card in DocumentDB `schema_catalog` contains:

- source and table identity;
- estimated row count;
- columns, SQL types, nullability, primary-key and foreign-key flags;
- outbound foreign-key edges with referenced table and column;
- bounded representative values collected by the existing catalog sampler;
- searchable card text and its local BGE-M3 embedding;
- `catalog_version`, `schema_hash`, and `captured_at` lifecycle fields.

`schema_catalog_state` stores one active generation per source:

```json
{
  "_id": "source-id",
  "active_version": "uuid-v7",
  "schema_hash": "sha256",
  "captured_at": "timestamp",
  "table_count": 12,
  "status": "active"
}
```

Credentials and source records are not copied into either metadata collection. All introspection, sampling, embedding, and storage remain on premises.

## Refresh lifecycle

A catalog refresh follows a last-known-good generation protocol:

1. Introspect the live source through its connector.
2. Build all table cards and foreign-key edges in memory.
3. Generate embeddings locally as one batch.
4. Compute a deterministic SHA-256 schema fingerprint.
5. Insert the complete new catalog generation.
6. Atomically move the source's active-version pointer.
7. Remove inactive generations as best-effort cleanup.

A failed introspection, embedding, or insert does not replace the active generation. Readers always filter by the active pointer. Legacy unversioned cards remain readable until their first successful refresh.

Refresh triggers:

- successful source creation or connection test;
- successful or partially successful ingestion completion;
- admin `POST /nl2sql/<source_id>/catalog/refresh`;
- lazy initialization for connected sources created before the catalog existed.

Source configuration changes invalidate cards and active state. Source deletion cascades to both collections.

## Management API

### Inspect active metadata

`GET /nl2sql/<source_id>/catalog` (admin)

Returns the active version, schema hash, capture timestamp, table count, and status.

### Force refresh

`POST /nl2sql/<source_id>/catalog/refresh` (admin)

Re-introspects the source and publishes a new catalog generation.

## Runtime relationship expansion

The linker first ranks table cards using exact table mentions and local embeddings. It then includes both ends of every one-hop foreign-key relationship connected to the selected tables. This prevents lookup tables from being omitted merely because their semantic score was below the top-k cutoff.

Example:

```text
patients.county_id -> counties.id
```

For “How many patients are from Nyeri?”, selecting `patients` also supplies `counties` to SQL generation and validation.

A conservative deterministic fast path handles geographic relationship counts when the schema provides a safe FK and lookup label column. It produces a validated read-only join such as:

```sql
SELECT COUNT(*) AS patient_count
FROM patients AS base
JOIN counties AS lookup ON base.county_id = lookup.id
WHERE LOWER(lookup.name) = LOWER('nyeri')
```

All generated statements still pass through the existing SQL AST validator, table allowlist, row cap, read-only connector, and execution timeout.

## Operational guidance

- Refresh after migrations that add, remove, or rename tables, columns, or foreign keys.
- Inspect the active hash and capture timestamp when SQL generation references stale structure.
- Keep database foreign keys declared where possible; undeclared relationships cannot be discovered reliably.
- Keep metadata sampling bounded and disable it where source policy prohibits representative values.
- Use role-aware column policies before enabling broad record listings in production.

## Automated health and drift detection

A background worker checks connected sources every `ONPREM_SCHEMA_POLL_INTERVAL_SECS` (default 300). Set it to `0` to disable polling. Work is bounded by `ONPREM_SCHEMA_POLL_CONCURRENCY` (default 2).

The poller hashes sorted structural metadata only. Row counts and profiles are excluded, so ordinary data growth does not trigger a rebuild. Structural drift publishes a new generation; unchanged checks avoid embedding work. Failures preserve the active generation and update health, last-check time, sanitized error text, and consecutive-failure count.

`schema_catalog_history` records manual, ingestion, source-lifecycle, lazy, and polling outcomes. `GET /nl2sql/<source_id>/catalog/history` returns the latest entries to administrators.

## Bounded column profiles

Each column card includes locally calculated, explicitly approximate statistics from at most `ONPREM_SCHEMA_PROFILE_SAMPLE_ROWS` rows (default 256):

- extrapolated distinct count;
- sampled null ratio;
- sampled numeric or date/time minimum and maximum;
- sampled row count.

These profiles guide planning without running expensive whole-table aggregates. They are refreshed with the catalog and are not included in the structural fingerprint.

## Curated metadata

Administrators can manage business aliases and undeclared relationships through:

- `GET /nl2sql/<source_id>/catalog/overrides`;
- `PUT /nl2sql/<source_id>/catalog/overrides`.

Overrides are stored separately in `schema_metadata_overrides`, survive catalog refreshes, and are validated against the active tables and columns. Alias and relationship edits are audited. Removing an entry in the admin editor and saving deletes that override.

## Runtime graph cache

The linker caches each source graph in memory by active catalog version. It merges declared foreign keys, curated relationship edges, and aliases when loading a graph. A version change, source update/deletion, or override save invalidates the relevant entry. This removes repeated DocumentDB card deserialization from the live SQL path while keeping changes immediately visible.

## Administrator UI

The connection metadata modal displays health, last background check, last refresh, current version/fingerprint, and recent check history. It also provides editors for business aliases and undeclared relationships. All calls continue through the Tauri bridge, so JWTs remain outside the React layer.
