# Synthetic hospital source database

The optional `dev-postgres` service is a stand-in for a hospital's operational
database, so ingestion, text-to-SQL and the agent tabs can be exercised end to end
without a real EMR.

| File | Purpose |
| --- | --- |
| [`init/01_schema.sql`](init/01_schema.sql) | 64-table hospital schema (organisation, staffing, scheduling, clinical, maternity, pharmacy stock, blood bank, safety, operations, revenue). |
| [`init/02_seed.sql`](init/02_seed.sql) | Generated data. **Do not edit by hand.** |
| [`generate_seed.py`](generate_seed.py) | Deterministic generator that writes `02_seed.sql`. Standard library only. |

## Regenerating the data

```bash
python docker/dev-postgres/generate_seed.py                     # 200 patients (default)
python docker/dev-postgres/generate_seed.py --patients 60       # smaller, faster to ingest
python docker/dev-postgres/generate_seed.py --seed 777          # a different hospital
```

The same `--seed` always produces a byte-identical file, so the seed can be
committed and diffed like any other source.

## Loading it

Init scripts only run on a **fresh** data directory. To reload after changing the
schema or regenerating the seed, reset this service's volume only — never
`docker compose down -v`, which would also wipe DocumentDB:

```bash
docker rm -f rag-dev-postgres
docker volume rm rag_dev-postgres-data
docker compose --profile dev-sources up -d dev-postgres
```

To replay just the seed against a running container:

```bash
docker cp docker/dev-postgres/init/02_seed.sql rag-dev-postgres:/tmp/02.sql
docker exec rag-dev-postgres psql -U health -d health_records -v ON_ERROR_STOP=1 -f /tmp/02.sql
```

The seed truncates every table first, so replaying it is idempotent.

**After reloading, re-ingest.** DocumentDB still holds chunks built from the previous
data; the aggregation catalog and the schema catalog are both derived from what was
ingested, so stale records will answer questions with stale facts.

## What the data models

A district hospital with a real spine rather than independent columns. Agent-facing table
ownership is documented in
[plans/docs/hospital-agents-and-data-map.md](../../plans/docs/hospital-agents-and-data-map.md).

- **Organisation** — 18 departments → 12 wards → 168 beds, each bed held by at most
  one open admission at a time.
- **Staffing** — 70 providers whose `role` constrains what they do: only doctors run
  clinics and admit, only nurses triage and sign the drug round, only lab staff verify
  results, only radiologists report imaging. Licences carry expiry dates; leave is
  recorded against the roster.
- **Scheduling** — weekly clinic sessions per doctor, and appointments booked into
  them with a full lifecycle (booked → confirmed → checked in → completed, plus
  cancellations, no-shows and reschedules). Completed bookings link to the encounter
  they produced, so "which patients are booked with Dr X" and "who did Dr X actually
  see" are different, answerable questions. Around 180 bookings are in the future.
- **Clinical** — triage with door-to-doctor timings, SOAP notes, diagnoses with
  certainty, orders and results computed against reference ranges (so abnormal flags
  follow the numbers), procedures, theatre lists with a named surgical team, ward
  rounds, transfers, referrals, and a medication administration record showing what
  was given, held, refused or missed.
- **Revenue** — bills summed from the items actually raised, insurance cover, claims
  with approval outcomes, and payments that never exceed the bill.
- **Maternity** — antenatal profiles, deliveries with mode and blood loss, and the babies born,
  linked back to the mother.
- **Pharmacy stock** — batches with expiry and quantity on hand, and every movement in or out
  traced to the prescription that consumed it.
- **Blood bank** — units by component and expiry, cross-matched and transfused, with reactions.
- **Safety and governance** — incident register, death certification with immediate and
  underlying cause, statutory disease notification, consent, patient feedback, and a record-access
  log including break-glass entries.
- **Operations** — ward duty rota, biomedical equipment with maintenance history, and chronic
  care programme registers (HIV, TB, diabetes, hypertension, MCH, palliative, mental health).

Clinical "today" is 2026-09-04; history starts 2023-01-02 and bookings run to
mid-October 2026. All amounts are in KES.

## Invariants the generator maintains

Worth knowing when grading an agent's answer — if the app contradicts one of these,
the app is wrong, not the data:

- No negative or impossible length of stay; discharge never precedes admission.
- No encounter before the patient was registered; no result before its order.
- No two patients in one bed.
- Bill subtotals equal the sum of their line items; payments never exceed the bill.
- Role integrity: no nurse recorded as the consulting doctor, no clerk triaging.
- No column is uniformly NULL, and no status column has only one value.
