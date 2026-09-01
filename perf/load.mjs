const baseUrl = (process.env.ONPREM_LOAD_URL ?? "http://127.0.0.1:8000").replace(/\/$/, "");
const concurrency = positiveInt(process.env.ONPREM_LOAD_CONCURRENCY, 10);
const requests = positiveInt(process.env.ONPREM_LOAD_REQUESTS, 50);
const timeoutMs = positiveInt(process.env.ONPREM_LOAD_TIMEOUT_MS, 120_000);
const question = process.env.ONPREM_LOAD_QUESTION ?? "What medications are listed?";

const login = await fetch(`${baseUrl}/auth/login`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({
    identifier: process.env.ONPREM_LOAD_USERNAME ?? "admin",
    password: process.env.ONPREM_LOAD_PASSWORD ?? "password",
  }),
});
if (!login.ok) throw new Error(`login failed: ${login.status}`);
const { token } = await login.json();

let next = 0;
const results = [];
await Promise.all(Array.from({ length: concurrency }, async () => {
  while (next < requests) {
    const id = next++;
    const started = performance.now();
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      const response = await fetch(`${baseUrl}/chat`, {
        method: "POST",
        headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
        body: JSON.stringify({ question }),
        signal: controller.signal,
      });
      await response.text();
      results[id] = { status: response.status, ms: performance.now() - started };
    } catch (error) {
      results[id] = { status: 0, ms: performance.now() - started, error: error.name };
    } finally {
      clearTimeout(timer);
    }
  }
}));

const latencies = results.map((result) => result.ms).sort((a, b) => a - b);
const statuses = Object.groupBy(results, (result) => String(result.status));
const report = {
  timestamp: new Date().toISOString(),
  base_url: baseUrl,
  concurrency,
  requests,
  status_counts: Object.fromEntries(Object.entries(statuses).map(([key, value]) => [key, value.length])),
  latency_ms: { p50: percentile(latencies, 0.5), p95: percentile(latencies, 0.95), p99: percentile(latencies, 0.99) },
};
console.log(JSON.stringify(report, null, 2));
if (results.some((result) => result.status >= 500 || result.status === 0)) process.exitCode = 1;

function percentile(values, fraction) {
  return Math.round(values[Math.ceil((values.length - 1) * fraction)] ?? 0);
}
function positiveInt(value, fallback) {
  const parsed = Number(value ?? fallback);
  if (!Number.isInteger(parsed) || parsed <= 0) throw new Error(`expected positive integer, got '${value}'`);
  return parsed;
}
