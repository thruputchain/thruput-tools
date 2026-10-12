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
  process.env.THRUPUT_CLI ?? join(repo, "target/debug/thruput"),
);
const work = await mkdtemp(join(tmpdir(), "groundhog-counter-app-"));
const project = join(work, "app");
const env = {
  ...process.env,
  XDG_CONFIG_HOME: join(work, "config"),
  THRUPUT_CLI: cli,
};
delete env.THRUPUT_PROFILE;
delete env.THRUPUT_DEFAULT_KEY;
delete env.THRUPUT_RUNTIME_MANIFEST;
delete env.THRUPUT_RPC_URL;
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
    `${await readFile(cargoConfig, "utf8")}\n[patch."sparse+https://crates.thruput.network/api/v1/crates/"]\nthruput-json-rpc-client = { path = ${JSON.stringify(join(repo, "crates/json-rpc/json-rpc-client"))} }\n`,
  );
  console.log("Installing generated app dependencies");
  await run("npm", ["install"]);
  console.log("Checking missing-deployment diagnostics");
  const missingDeployment = start(process.execPath, ["scripts/dev.mjs"]);
  assert.notEqual(await missingDeployment.closed, 0);
  assert.match(missingDeployment.stderr, /npm run contract:deploy/);
  console.log("Checking missing CLI and initialization diagnostics");
  const missingCli = start(process.execPath, ["scripts/deploy.mjs"], project, {
    THRUPUT_CLI: join(work, "missing-groundhog"),
  });
  assert.notEqual(await missingCli.closed, 0);
  assert.match(missingCli.stderr, /Install the Thruput CLI/);
  const missingInit = start(process.execPath, ["scripts/deploy.mjs"]);
  assert.notEqual(await missingInit.closed, 0);
  assert.match(missingInit.stderr, /thruput init/);
  await run(cli, ["init", "--generate-key", "--non-interactive", "--json"]);
  console.log("Checking missing-node diagnostics");
  const missingNode = start(process.execPath, ["scripts/deploy.mjs"], project, {
    THRUPUT_RPC_URL: `http://127.0.0.1:${await unusedPort()}`,
  });
  assert.notEqual(await missingNode.closed, 0);
  assert.match(missingNode.stderr, /thruput node in another terminal/);

  console.log("Building contract and bindings without a node");
  await run("npm", ["run", "contract:build"]);
  await assert.rejects(readFile(join(project, ".thruput/app.json")), {
    code: "ENOENT",
  });

  const nodeArgs = ["node", "--port", "0", "--json", "--non-interactive"];
  if (env.THRUPUT_DEVNODE_BINARY)
    nodeArgs.push(
      "--hog-devnode-binary",
      resolve(env.THRUPUT_DEVNODE_BINARY),
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

  console.log("Checking explicit deployment");
  await run("npm", ["run", "contract:deploy"], project, {
    THRUPUT_RPC_URL: rpcUrl,
  });
  const deploymentPath = join(project, ".thruput/app.json");
  const savedDeployment = await readFile(deploymentPath, "utf8");
  const deployment = JSON.parse(savedDeployment);
  assert.equal(deployment.rpcUrl, rpcUrl);

  console.log("Checking saved-deployment diagnostics");
  await writeFile(deploymentPath, "{}");
  const invalidDeployment = start(process.execPath, ["scripts/dev.mjs"]);
  assert.notEqual(await invalidDeployment.closed, 0);
  assert.match(
    invalidDeployment.stderr,
    /Invalid saved deployment.*contract:deploy/,
  );
  await writeFile(
    deploymentPath,
    JSON.stringify({ ...deployment, txHash: `0x${"00".repeat(32)}` }),
  );
  const staleDeployment = start(process.execPath, ["scripts/dev.mjs"]);
  assert.notEqual(await staleDeployment.closed, 0);
  assert.match(
    staleDeployment.stderr,
    /unavailable on this node.*contract:deploy/,
  );
  await writeFile(deploymentPath, savedDeployment);

  console.log("Checking generated contract and frontend builds");
  await run("cargo", ["clippy", "--all-targets", "--", "-D", "warnings"]);
  await run("cargo", ["test"]);
  await run(cli, ["test", "--non-interactive"]);
  await run("npm", ["run", "build"]);
  assert.equal(
    await readFile(deploymentPath, "utf8"),
    savedDeployment,
    "build must not change deployment",
  );

  const port = await unusedPort();
  const url = `http://127.0.0.1:${port}`;
  async function startFrontend() {
    app = start("npm", ["run", "dev"], project, {
      // Frontend startup must not invoke the CLI, even when it is unavailable.
      THRUPUT_CLI: join(work, "missing-groundhog"),
      PORT: String(port),
    });
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
    );
  }
  async function servedDeployment() {
    const response = await fetch(`${url}/deployment.json`);
    assert.equal(response.status, 200);
    return response.json();
  }
  console.log("Checking frontend restart without deployment");
  await startFrontend();
  assert.deepEqual(await servedDeployment(), deployment);
  await stop(app);
  await startFrontend();
  assert.deepEqual(await servedDeployment(), deployment);
  assert.equal(await readFile(deploymentPath, "utf8"), savedDeployment);

  await writeFile(
    deploymentPath,
    JSON.stringify({
      ...deployment,
      rpcUrl: `http://127.0.0.1:${await unusedPort()}`,
    }),
  );
  const changedEndpoint = await fetch(`${url}/deployment.json`);
  assert.equal(changedEndpoint.status, 503);
  assert.match(
    (await changedEndpoint.json()).error,
    /endpoint changed.*Restart npm run dev/,
  );
  await writeFile(deploymentPath, savedDeployment);

  console.log("Redeploying while the frontend keeps running");
  await run("npm", ["run", "contract:deploy"], project, {
    THRUPUT_RPC_URL: rpcUrl,
  });
  const redeployed = await servedDeployment();
  assert.notEqual(redeployed.contractAddress, deployment.contractAddress);
  assert.equal(app.done, false);

  console.log("Checking recovery after an ephemeral node reset");
  await stop(node);
  const restartedArgs = [...nodeArgs];
  restartedArgs[restartedArgs.indexOf("--port") + 1] = new URL(rpcUrl).port;
  node = start(cli, restartedArgs);
  await waitFor(() => node.stdout.includes("\n"), node, "restarted node");
  assert.equal(
    JSON.parse(node.stdout.trim().split("\n")[0]).ok,
    true,
    node.stderr,
  );
  const staleResponse = await fetch(`${url}/deployment.json`);
  assert.equal(staleResponse.status, 503);
  assert.match(
    (await staleResponse.json()).error,
    /unavailable on this node.*contract:deploy/,
  );
  await run("npm", ["run", "contract:deploy"], project, {
    THRUPUT_RPC_URL: rpcUrl,
  });
  const recovered = await servedDeployment();
  assert.notEqual(recovered.contractAddress, redeployed.contractAddress);
  const browserDeployment = await readFile(deploymentPath, "utf8");
  await stop(app);
  assert.equal(node.done, false, "app shutdown must leave the node running");
  console.log("Deployment lifecycle checks passed");

  browser = await chromium.launch({
    headless: true,
    executablePath: process.env.CHROMIUM_EXECUTABLE,
  });
  const page = await browser.newPage();
  const pageErrors = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  let lastView;
  let staleView;
  let staleReads = 0;
  let submissions = 0;
  let views = 0;
  await page.route("**/rpc", async (route) => {
    const request = route.request().postDataJSON();
    if (request.method === "groundhog_sendRawTransaction") submissions++;
    if (request.method !== "groundhog_view") return route.continue();
    views++;
    const response = await route.fetch();
    const body = await response.json();
    lastView = body.result;
    if (staleReads > 0) {
      staleReads--;
      return route.fulfill({ response, json: { ...body, result: staleView } });
    }
    return route.fulfill({ response });
  });
  async function walletReady() {
    await waitFor(
      async () => {
        if (await page.getByRole("alert").isVisible())
          throw new Error(
            `Browser startup failed: ${await page.getByRole("alert").textContent()}`,
          );
        return page
          .getByRole("button", { name: "Increment", exact: true })
          .isEnabled();
      },
      app,
      "browser wallet",
    );
  }
  let firstAddress;
  for (let attempt = 0; attempt < 2; attempt++) {
    console.log(`Starting browser session ${attempt + 1}`);
    await startFrontend();
    await page.goto(url);
    await page
      .getByRole("button", { name: "Increment", exact: true })
      .waitFor();
    await walletReady();
    const initialValue = attempt === 1 ? "1" : "0";
    const updatedValue = attempt === 1 ? "2" : "1";
    assert.equal(
      await page.getByTestId("counter-value").textContent(),
      initialValue,
    );
    const address = await page.getByTestId("contract-address").textContent();
    if (attempt === 0) firstAddress = address;
    else {
      assert.equal(
        address,
        firstAddress,
        "frontend restart must preserve the contract",
      );
      assert.equal(await readFile(deploymentPath, "utf8"), browserDeployment);
    }

    if (attempt !== 1) {
      await page
        .getByRole("button", { name: "Decrement", exact: true })
        .click();
      await page.getByRole("alert").waitFor();
      assert.match(
        await page.getByRole("alert").textContent(),
        /failed|underflow|abort/i,
      );
      assert.equal(await page.getByTestId("counter-value").textContent(), "0");
    }
    // Reproduce a view backend lagging behind a successful receipt. Only reads
    // are held back; the transaction is submitted and executed by the real node.
    staleView = structuredClone(lastView);
    staleReads = 3;
    const submissionsBefore = submissions;
    const viewsBefore = views;
    await page.getByRole("button", { name: "Increment", exact: true }).click();
    await waitFor(
      async () => {
        if (await page.getByRole("alert").isVisible()) {
          throw new Error(
            `Increment failed: ${await page.getByRole("alert").textContent()}`,
          );
        }
        return (
          (await page.getByTestId("counter-value").textContent()) === updatedValue
        );
      },
      app,
      "increment",
    );
    assert.match(
      await page.getByRole("status").textContent(),
      /executed successfully/,
    );
    assert.equal(staleReads, 0, "refresh must read past all stale snapshots");
    assert.ok(views >= viewsBefore + 4);
    assert.equal(
      submissions,
      submissionsBefore + 1,
      "refresh must not resubmit the transaction",
    );
    const viewsAfter = views;
    await delay(600);
    assert.equal(views, viewsAfter, "refresh must stop once caught up");
    const wallet = await page.getByTestId("wallet-address").textContent();
    await page.reload();
    await walletReady();
    assert.notEqual(
      await page.getByTestId("wallet-address").textContent(),
      wallet,
    );
    assert.equal(
      await page.getByTestId("contract-address").textContent(),
      address,
    );
    assert.equal(
      await page.getByTestId("counter-value").textContent(),
      updatedValue,
    );
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
    if (attempt === 1) {
      staleView = structuredClone(lastView);
      staleReads = Infinity;
      const beforeTimeout = submissions;
      await page.getByRole("button", { name: "Increment", exact: true }).click();
      await waitFor(
        () => page.getByRole("alert").isVisible(),
        app,
        "refresh timeout",
        15_000,
      );
      assert.match(
        await page.getByRole("alert").textContent(),
        /Transaction executed successfully, but the value could not be refreshed/,
      );
      assert.equal(submissions, beforeTimeout + 1);
      const stoppedViews = views;
      await delay(600);
      assert.equal(views, stoppedViews, "a timed-out refresh must stop reading");
      staleReads = 0;
      await page.reload();
      await walletReady();
      assert.equal(
        await page.getByTestId("counter-value").textContent(),
        (BigInt(updatedValue) + 1n).toString(),
      );
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
  console.log(
    "Counter app smoke passed: explicit deployment, restart persistence, redeployment, node reset, execution, reload, responsive layout, cleanup, and builds.",
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
