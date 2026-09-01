import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const args = parseArgs(process.argv.slice(2));
const suite = args.suite ?? "full";
const smoke = Boolean(args.smoke);
const baseUrl = (args.baseUrl ?? process.env.ONPREM_EVAL_URL ?? "http://127.0.0.1:8000").replace(/\/$/, "");
const username = process.env.ONPREM_EVAL_USERNAME ?? "admin";
const password = process.env.ONPREM_EVAL_PASSWORD ?? "password";
const timeoutMs = Number(args.timeoutMs ?? process.env.ONPREM_EVAL_TIMEOUT_MS ?? 30000);
const config = args.config ?? "full";

if (args.validateOnly) {
  const router = await loadJsonl("router.jsonl");
  const retrieval = await loadJsonl("retrieval.jsonl");
  console.log(`Validated ${router.length} router and ${retrieval.length} retrieval fixtures.`);
  process.exit(0);
}

if (!["router", "retrieval", "full"].includes(suite)) {
  fail(`unknown suite '${suite}'; expected router, retrieval, or full`);
}
if (!["full", "naive", "no-rerank", "fast"].includes(config)) {
  fail(`unknown config '${config}'; expected full, naive, no-rerank, or fast`);
}

const token = await login();
const results = [];
if (suite === "router" || suite === "full") results.push(await runRouter());
if (suite === "retrieval" || suite === "full") results.push(await runRetrieval());

const failed = results.some((result) => !result.passed);
await writeReport(results, failed);
for (const result of results) printResult(result);
process.exitCode = failed ? 1 : 0;

async function login() {
  const response = await request("/auth/login", {
    identifier: username,
    password,
  }, false);
  if (typeof response.token !== "string" || response.token.length === 0) {
    fail("login response did not contain a token");
  }
  return response.token;
}

async function runRouter() {
  const fixtures = selectFixtures(await loadJsonl("router.jsonl"));
  const cases = [];
  for (const fixture of fixtures) {
    const started = performance.now();
    const actual = await request("/route", {
      question: fixture.question,
      has_history: Boolean(fixture.has_history),
    });
    const routeMatch = actual.route === fixture.route_expected;
    const intentMatch = (actual.intent ?? null) === (fixture.intent_expected ?? null);
    const tierMatch = fixture.tier_expected === undefined || actual.tier === fixture.tier_expected;
    cases.push({
      id: fixture.id,
      passed: routeMatch && intentMatch && tierMatch,
      expected: `${fixture.route_expected}/${fixture.intent_expected ?? "none"}`,
      actual: `${actual.route}/${actual.intent ?? "none"}`,
      tier: actual.tier,
      duration_ms: Math.round(performance.now() - started),
    });
  }
  const accuracy = ratio(cases.filter((item) => item.passed).length, cases.length);
  const threshold = Number(args.routerThreshold ?? 0.95);
  return { suite: "router", passed: accuracy >= threshold, metrics: { accuracy, threshold, cases: cases.length }, cases };
}

async function runRetrieval() {
  const fixtures = selectFixtures(await loadJsonl("retrieval.jsonl"));
  const cases = [];
  const options = retrievalOptions(config);
  for (const fixture of fixtures) {
    const started = performance.now();
    const actual = await request("/search", {
      query: fixture.query,
      top_k: fixture.top_k ?? 6,
      ...options,
    });
    const passages = Array.isArray(actual.passages) ? actual.passages : [];
    const index = passages.findIndex((passage) =>
      typeof passage.id === "string" && passage.id.includes(fixture.expected_id_contains),
    );
    const rank = index < 0 ? null : index + 1;
    cases.push({
      id: fixture.id,
      passed: rank !== null,
      rank,
      reciprocal_rank: rank === null ? 0 : 1 / rank,
      returned: passages.length,
      duration_ms: Math.round(performance.now() - started),
    });
  }
  const hitRate = ratio(cases.filter((item) => item.passed).length, cases.length);
  const mrr = ratio(cases.reduce((sum, item) => sum + item.reciprocal_rank, 0), cases.length);
  const hitRateThreshold = Number(args.hitRateThreshold ?? 1);
  const mrrThreshold = Number(args.mrrThreshold ?? 0.5);
  return {
    suite: "retrieval",
    passed: hitRate >= hitRateThreshold && mrr >= mrrThreshold,
    metrics: { hit_rate_at_6: hitRate, mrr, hit_rate_threshold: hitRateThreshold, mrr_threshold: mrrThreshold, cases: cases.length, config },
    cases,
  };
}

async function request(path, body, authenticated = true) {
  let response;
  try {
    response = await fetch(`${baseUrl}${path}`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        ...(authenticated ? { authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch (error) {
    fail(`${path} request failed: ${error.message}`);
  }
  const text = await response.text();
  if (!response.ok) fail(`${path} returned HTTP ${response.status}: ${text.slice(0, 300)}`);
  try {
    return JSON.parse(text);
  } catch {
    fail(`${path} returned invalid JSON`);
  }
}

async function loadJsonl(name) {
  const text = await readFile(join(root, "data", name), "utf8");
  return text.split(/\r?\n/).filter((line) => line.trim()).map((line, index) => {
    try {
      return JSON.parse(line);
    } catch (error) {
      fail(`${name}:${index + 1}: ${error.message}`);
    }
  });
}

function selectFixtures(fixtures) {
  return smoke ? fixtures.slice(0, 5) : fixtures;
}

function retrievalOptions(name) {
  if (name === "naive") return { mode: "vector", rerank: false };
  if (name === "no-rerank") return { mode: "hybrid", rerank: false };
  if (name === "fast") return { mode: "vector", rerank: true };
  return { mode: "hybrid", rerank: true };
}

async function writeReport(runResults, failed) {
  const now = new Date();
  const stamp = now.toISOString().replaceAll(":", "-");
  const reportsDir = join(root, "reports");
  await mkdir(reportsDir, { recursive: true });
  const report = {
    generated_at: now.toISOString(),
    base_url: baseUrl,
    suite,
    smoke,
    config,
    passed: !failed,
    results: runResults,
  };
  const path = join(reportsDir, `${stamp}-${suite}-${config}.json`);
  await writeFile(path, `${JSON.stringify(report, null, 2)}\n`, "utf8");
  console.log(`Report: ${path}`);
}

function printResult(result) {
  const status = result.passed ? "PASS" : "FAIL";
  console.log(`\n${status} ${result.suite}: ${JSON.stringify(result.metrics)}`);
  for (const item of result.cases.filter((entry) => !entry.passed)) {
    console.log(`  FAIL ${item.id}: ${JSON.stringify(item)}`);
  }
}

function ratio(numerator, denominator) {
  return denominator === 0 ? 0 : numerator / denominator;
}

function parseArgs(values) {
  const parsed = {};
  for (let index = 0; index < values.length; index += 1) {
    const value = values[index];
    if (value === "--smoke" || value === "--validate-only") {
      parsed[value === "--smoke" ? "smoke" : "validateOnly"] = true;
      continue;
    }
    if (!value.startsWith("--")) fail(`unexpected argument '${value}'`);
    const key = value.slice(2).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
    const next = values[index + 1];
    if (!next || next.startsWith("--")) fail(`missing value for '${value}'`);
    parsed[key] = next;
    index += 1;
  }
  return parsed;
}

function fail(message) {
  console.error(`eval: ${message}`);
  process.exit(2);
}
