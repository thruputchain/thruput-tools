import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { chromium } from "playwright";

const repo = fileURLToPath(new URL("../../", import.meta.url));
const cli = resolve(
  process.env.GROUNDHOG_CLI ?? join(repo, "target/debug/groundhog"),
);
const work = await mkdtemp(join(tmpdir(), "groundhog-counter-app-"));
const project = join(work, "app");
const env = {
  ...process.env,
  XDG_CONFIG_HOME: join(work, "config"),
  GROUNDHOG_CLI: cli,
};
delete env.GROUNDHOG_PROFILE;
delete env.GROUNDHOG_DEFAULT_KEY;
delete env.GROUNDHOG_RUNTIME_MANIFEST;
delete env.GROUNDHOG_RPC_URL;
delete env.PORT;
let node, app, browser;
console.log(`Scaffold app smoke: ${work}`);

function start(executable, args, cwd = project, overrides = {}) {
  const process = spawn(executable, args, {
    cwd,
    env: { ...env, ...overrides },
    stdio: ["ignore", "pipe", "pipe"],
    detached: true,
  });
  const state = { process, stdout: "", stderr: "", done: false };
  process.stdout.on("data", (chunk) => {
    state.stdout += chunk;
  });
  process.stderr.on("data", (chunk) => {
    state.stderr += chunk;
  });
  state.closed = once(process, "close").then(([code]) => {
    state.done = true;
    return code;
  });
  return state;
}

async function run(executable, args, cwd = project, overrides = {}) {
  const state = start(executable, args, cwd, overrides);
  const code = await state.closed;
  assert.equal(
    code,
    0,
    `${executable} ${args.join(" ")}\n${state.stdout}\n${state.stderr}`,
  );
  return state.stdout;
}

async function stop(state) {
  if (!state || state.done) return;
  process.kill(-state.process.pid, "SIGTERM");
  const timer = setTimeout(() => {
    try {
      process.kill(-state.process.pid, "SIGKILL");
    } catch (error) {
      if (error.code !== "ESRCH") throw error;
    }
  }, 10_000);
  try {
    await state.closed;
  } finally {
    clearTimeout(timer);
  }
}

async function waitFor(test, state, label, timeout = 180_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await test()) return;
    if (state?.done)
      throw new Error(`${label} exited\n${state.stdout}\n${state.stderr}`);
    await delay(100);
  }
  throw new Error(`Timed out: ${label}\n${state?.stdout}\n${state?.stderr}`);
}

async function unusedPort() {
  const server = createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

try {
  await run(
    cli,
    [
      "new",
      "counter-example",
      "--template",
      "counter-app",
      "--path",
      project,
      "--non-interactive",
    ],
    work,
  );
  // Release PRs may reference an unpublished Rust client. Exercise this checkout.
  const cargoConfig = join(project, ".cargo/config.toml");
  await writeFile(
    cargoConfig,
    `${await readFile(cargoConfig, "utf8")}\n[patch."sparse+https://crates.thruput.network/api/v1/crates/"]\ngroundhog-json-rpc-client = { path = ${JSON.stringify(join(repo, "crates/json-rpc/json-rpc-client"))} }\n`,
  );
  console.log("Installing generated app dependencies");
  await run("npm", ["install"]);
  console.log("Checking missing CLI and initialization diagnostics");
  const missingCli = start(process.execPath, ["scripts/dev.mjs"], project, {
    GROUNDHOG_CLI: join(work, "missing-groundhog"),
  });
  assert.notEqual(await missingCli.closed, 0);
  assert.match(missingCli.stderr, /Install the Groundhog CLI/);
  const missingInit = start(process.execPath, ["scripts/dev.mjs"]);
  assert.notEqual(await missingInit.closed, 0);
  assert.match(missingInit.stderr, /groundhog init/);
  await run(cli, ["init", "--generate-key", "--non-interactive", "--json"]);
  console.log("Checking missing-node diagnostics");
  const missingNode = start(process.execPath, ["scripts/dev.mjs"], project, {
    GROUNDHOG_RPC_URL: `http://127.0.0.1:${await unusedPort()}`,
  });
  assert.notEqual(await missingNode.closed, 0);
  assert.match(missingNode.stderr, /groundhog node in another terminal/);

  const nodeArgs = ["node", "--port", "0", "--json", "--non-interactive"];
  if (env.GROUNDHOG_DEVNODE_BINARY)
    nodeArgs.push(
      "--hog-devnode-binary",
      resolve(env.GROUNDHOG_DEVNODE_BINARY),
    );
  if (env.HOGD_PROTOCOL_OVERRIDE)
    nodeArgs.push("--hogd-protocol", env.HOGD_PROTOCOL_OVERRIDE);
  node = start(cli, nodeArgs);
  await waitFor(() => node.stdout.includes("\n"), node, "local node");
  const started = JSON.parse(node.stdout.trim().split("\n")[0]);
  assert.equal(started.ok, true, node.stderr);
  const rpcUrl = started.data.rpc_url;
  assert.equal(typeof rpcUrl, "string");
  console.log(`Local node ready: ${rpcUrl}`);

  browser = await chromium.launch({
    headless: true,
    executablePath: process.env.CHROMIUM_EXECUTABLE,
  });
  const page = await browser.newPage();
  const pageErrors = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  let firstAddress;
  for (let attempt = 0; attempt < 2; attempt++) {
    const port = await unusedPort();
    const url = `http://127.0.0.1:${port}`;
    app = start("npm", ["run", "dev"], project, {
      GROUNDHOG_RPC_URL: rpcUrl,
      PORT: String(port),
    });
    console.log(`Starting app session ${attempt + 1}`);
    await waitFor(
      async () => {
        try {
          return (await fetch(`${url}/deployment.json`)).ok;
        } catch {
          return false;
        }
      },
      app,
      "app startup",
      600_000,
    );
    await page.goto(url);
    await page
      .getByRole("button", { name: "Increment", exact: true })
      .waitFor();
    await waitFor(
      () =>
        page
          .getByRole("button", { name: "Increment", exact: true })
          .isEnabled(),
      app,
      "browser wallet",
    );
    assert.equal(await page.getByTestId("counter-value").textContent(), "0");
    const address = await page.getByTestId("contract-address").textContent();
    if (attempt === 0) firstAddress = address;
    else
      assert.notEqual(
        address,
        firstAddress,
        "each dev-server run deploys fresh",
      );

    await page.getByRole("button", { name: "Decrement", exact: true }).click();
    await page.getByRole("alert").waitFor();
    assert.match(
      await page.getByRole("alert").textContent(),
      /failed|underflow|abort/i,
    );
    assert.equal(await page.getByTestId("counter-value").textContent(), "0");
    await page.getByRole("button", { name: "Increment", exact: true }).click();
    await waitFor(
      async () =>
        (await page.getByTestId("counter-value").textContent()) === "1",
      app,
      "increment",
    );
    assert.match(
      await page.getByRole("status").textContent(),
      /executed successfully/,
    );
    const wallet = await page.getByTestId("wallet-address").textContent();
    await page.reload();
    await waitFor(
      () =>
        page
          .getByRole("button", { name: "Increment", exact: true })
          .isEnabled(),
      app,
      "reloaded wallet",
    );
    assert.notEqual(
      await page.getByTestId("wallet-address").textContent(),
      wallet,
    );
    assert.equal(
      await page.getByTestId("contract-address").textContent(),
      address,
    );
    assert.equal(await page.getByTestId("counter-value").textContent(), "1");
    for (const width of [375, 1280]) {
      await page.setViewportSize({ width, height: 900 });
      assert.ok(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth,
        ),
      );
      await page.screenshot({
        path: join(work, `counter-${attempt}-${width}.png`),
        fullPage: true,
      });
    }
    await stop(app);
    assert.equal(node.done, false, "app shutdown must leave the node running");
    await waitFor(
      async () => {
        try {
          await fetch(url);
          return false;
        } catch {
          return true;
        }
      },
      undefined,
      "app shutdown",
      10_000,
    );
  }
  assert.deepEqual(pageErrors, []);
  console.log("Checking generated contract and frontend builds");
  await run("cargo", ["clippy", "--all-targets", "--", "-D", "warnings"]);
  await run("cargo", ["test"]);
  await run(cli, ["test", "--non-interactive"]);
  await run("npm", ["run", "build"]);
  console.log(
    "Counter app smoke passed: rejection, execution, reload, fresh deployment, responsive layout, cleanup, and builds.",
  );
} finally {
  await browser?.close();
  await stop(app);
  await stop(node);
  if (node)
    await writeFile(join(work, "node.log"), `${node.stdout}\n${node.stderr}`);
  if (app)
    await writeFile(join(work, "app.log"), `${app.stdout}\n${app.stderr}`);
  console.log(`Artifacts retained at ${work}`);
}
