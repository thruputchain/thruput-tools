// `npm audit --audit-level=high` with an allowlist, which npm itself lacks.
// Fails on any high or critical advisory that is not listed in IGNORED, and
// on a listed entry that no longer matches anything.
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

// GHSA id -> the package the advisory is filed against.
export const IGNORED = new Map([
  // picomatch ReDoS (<=4.0.7; fix is 4.0.8, not yet published). picomatch is
  // Vite's glob matcher here: it only sees globs from this repository's own
  // test harness. Remove once package-lock.json carries picomatch >=4.0.8.
  ["GHSA-fc5q-39xc-vjpf", "picomatch"],
]);

// Sorts the advisories of an `npm audit --json` report into the ignored ids,
// the blocking advisories, and the allowlist entries that matched nothing.
export function evaluate(report, allowlist = IGNORED) {
  const blocking = new Map();
  const ignored = new Set();
  for (const { via } of Object.values(report.vulnerabilities ?? {})) {
    // String entries name another vulnerable package; its own entry carries
    // the advisory.
    for (const advisory of via.filter((entry) => typeof entry === "object")) {
      if (!["high", "critical"].includes(advisory.severity)) continue;
      const id = advisory.url?.split("/").pop();
      if (id !== undefined && allowlist.get(id) === advisory.name) {
        ignored.add(id);
      } else {
        blocking.set(advisory.url ?? advisory.title, advisory);
      }
    }
  }
  const stale = [...allowlist.keys()].filter((id) => !ignored.has(id));
  return {
    ignored: [...ignored],
    blocking: [...blocking.values()],
    stale,
    ok: blocking.size === 0 && stale.length === 0,
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const run = spawnSync("npm", ["audit", "--json"], {
    cwd: import.meta.dirname,
    encoding: "utf8",
  });
  if (run.error) throw run.error;
  const report = JSON.parse(run.stdout);
  if (report.error) {
    throw new Error(`npm audit: ${JSON.stringify(report.error)}`);
  }

  const result = evaluate(report);
  for (const id of result.ignored) {
    console.log(`ignored: ${id} (${IGNORED.get(id)})`);
  }
  for (const advisory of result.blocking) {
    console.error(
      `${advisory.severity}: ${advisory.name}: ${advisory.title} ${advisory.url}`,
    );
  }
  for (const id of result.stale) {
    console.error(`stale allowlist entry: ${id}`);
  }
  if (!result.ok) process.exit(1);
}
