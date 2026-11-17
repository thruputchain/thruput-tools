import assert from "node:assert/strict";
import test from "node:test";

import { evaluate } from "./audit.mjs";

const ALLOWLIST = new Map([["GHSA-aaaa-bbbb-cccc", "globber"]]);

const advisory = (name, id, severity = "high") => ({
  name,
  severity,
  title: `${name} problem`,
  url: `https://github.com/advisories/${id}`,
});

// The shape of `npm audit --json`: a package's `via` holds its own advisories
// and the names of vulnerable packages it depends on.
const report = (...advisories) => ({
  vulnerabilities: {
    ...Object.fromEntries(
      advisories.map((entry) => [entry.name, { via: [entry] }]),
    ),
    bundler: { via: advisories.map((entry) => entry.name) },
  },
});

test("an allowlisted advisory passes", () => {
  const result = evaluate(
    report(advisory("globber", "GHSA-aaaa-bbbb-cccc")),
    ALLOWLIST,
  );
  assert.equal(result.ok, true);
  assert.deepEqual(result.ignored, ["GHSA-aaaa-bbbb-cccc"]);
  assert.deepEqual(result.blocking, []);
});

test("an unrelated high advisory blocks beside an allowlisted one", () => {
  const other = advisory("parser", "GHSA-dddd-eeee-ffff", "critical");
  const result = evaluate(
    report(advisory("globber", "GHSA-aaaa-bbbb-cccc"), other),
    ALLOWLIST,
  );
  assert.equal(result.ok, false);
  assert.deepEqual(result.blocking, [other]);
  assert.deepEqual(result.stale, []);
});

test("the allowlisted id against another package blocks", () => {
  const moved = advisory("parser", "GHSA-aaaa-bbbb-cccc");
  const result = evaluate(report(moved), ALLOWLIST);
  assert.equal(result.ok, false);
  assert.deepEqual(result.blocking, [moved]);
  assert.deepEqual(result.stale, ["GHSA-aaaa-bbbb-cccc"]);
});

test("an allowlist entry that matches nothing fails as stale", () => {
  for (const clean of [{}, { vulnerabilities: {} }, report()]) {
    const result = evaluate(clean, ALLOWLIST);
    assert.equal(result.ok, false);
    assert.deepEqual(result.blocking, []);
    assert.deepEqual(result.stale, ["GHSA-aaaa-bbbb-cccc"]);
  }
});

test("advisories below high do not block", () => {
  const result = evaluate(
    report(
      advisory("globber", "GHSA-aaaa-bbbb-cccc"),
      advisory("parser", "GHSA-dddd-eeee-ffff", "moderate"),
    ),
    ALLOWLIST,
  );
  assert.equal(result.ok, true);
});

test("an advisory with no URL blocks", () => {
  const unnamed = { name: "globber", severity: "high", title: "unnamed" };
  const result = evaluate(report(unnamed), new Map());
  assert.equal(result.ok, false);
  assert.deepEqual(result.blocking, [unnamed]);
});
