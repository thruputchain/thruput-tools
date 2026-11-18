// `npm audit --audit-level=high` with an allowlist, which npm itself lacks.
// Fails on any high or critical advisory that is not listed in IGNORED.
import { spawnSync } from "node:child_process";

// Entries map an advisory ID to its package name, for example
// ["GHSA-xxxx-xxxx-xxxx", "picomatch"]. Remove an entry when the advisory no
// longer applies: a stale entry fails the audit.
const IGNORED = new Map();

const run = spawnSync("npm", ["audit", "--json"], {
  cwd: import.meta.dirname,
  encoding: "utf8",
});
if (run.error) throw run.error;
const report = JSON.parse(run.stdout);
if (report.error) throw new Error(`npm audit: ${JSON.stringify(report.error)}`);

const blocking = new Map();
const ignored = new Set();
for (const { via } of Object.values(report.vulnerabilities ?? {})) {
  // String entries name another vulnerable package; its own entry carries
  // the advisory.
  for (const advisory of via.filter((entry) => typeof entry === "object")) {
    if (!["high", "critical"].includes(advisory.severity)) continue;
    const id = advisory.url?.split("/").pop();
    if (IGNORED.get(id) === advisory.name) ignored.add(id);
    else blocking.set(advisory.url ?? advisory.title, advisory);
  }
}

for (const id of ignored) console.log(`ignored: ${id} (${IGNORED.get(id)})`);
for (const [key, advisory] of blocking) {
  console.error(`${advisory.severity}: ${advisory.name}: ${advisory.title} ${key}`);
}
for (const id of IGNORED.keys()) {
  if (!ignored.has(id)) console.error(`stale allowlist entry: ${id}`);
}
if (blocking.size > 0 || ignored.size !== IGNORED.size) process.exit(1);
