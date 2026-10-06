import assert from "node:assert/strict";
import { after, test } from "node:test";
import { copyFile, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { setTimeout as delay } from "node:timers/promises";

// Exercise the actual template as TypeScript without installing a generated app.
const directory = await mkdtemp(join(tmpdir(), "counter-refresh-test-"));
after(() => rm(directory, { recursive: true, force: true }));
const path = join(directory, "refresh-counter.ts");
await copyFile(
  new URL("../../crates/cli/templates/counter-app/refresh-counter.ts.tmpl", import.meta.url),
  path,
);
const { refreshCounter } = await import(pathToFileURL(path).href);

test("a current view returns immediately and schedules no further reads", async () => {
  let reads = 0;
  const value = await refreshCounter(async () => {
    reads++;
    return { height: 6n, value: 2n };
  }, 6n);
  assert.equal(value, 2n);
  await delay(250);
  assert.equal(reads, 1);
});

test("stale views are read sequentially until the receipt height is visible", async () => {
  const heights = [5n, 5n, 6n];
  let reads = 0;
  let active = 0;
  let maximumActive = 0;
  const value = await refreshCounter(async () => {
    active++;
    maximumActive = Math.max(maximumActive, active);
    await delay(5);
    const height = heights[reads++];
    active--;
    // Even a stale view with the expected value must not end the refresh.
    return { height, value: 2n };
  }, 6n);
  assert.equal(value, 2n);
  assert.equal(reads, 3);
  assert.equal(maximumActive, 1);
  await delay(250);
  assert.equal(reads, 3);
});

test("newer snapshots and heights beyond Number precision are accepted", async () => {
  const height = 9_007_199_254_740_993n;
  const value = await refreshCounter(
    async () => ({ height: height + 1n, value: 7n }),
    height,
  );
  assert.equal(value, 7n);
});

test("a genuine read failure is reported without retrying", async () => {
  let reads = 0;
  const error = new Error("RPC unavailable");
  await assert.rejects(refreshCounter(async () => {
    reads++;
    throw error;
  }, 6n), error);
  assert.equal(reads, 1);
});

test("a view that stays behind times out and stops reading", async () => {
  let reads = 0;
  await assert.rejects(refreshCounter(async () => {
    reads++;
    return { height: 5n, value: 1n };
  }, 6n, 20), /did not catch up/);
  const finished = reads;
  await delay(250);
  assert.equal(reads, finished);
});

test("the deadline aborts an in-flight read", async () => {
  let aborted = false;
  await assert.rejects(refreshCounter((signal) => new Promise((_resolve, reject) => {
    signal.addEventListener("abort", () => {
      aborted = true;
      reject(signal.reason);
    }, { once: true });
  }), 6n, 20), /did not catch up/);
  assert.equal(aborted, true);
});
