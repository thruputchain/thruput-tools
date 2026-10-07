import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer as createHttpServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { createServer } from "vite";

const template = await readFile(
  new URL("../../crates/cli/templates/counter-app/dev.mjs.tmpl", import.meta.url),
  "utf8",
);

for (const endpoint of ["/", "/groundhog", "/tenant/rpc?token=a%2Fb&network=local"]) {
  test(`dev proxy preserves RPC endpoint ${endpoint}`, { timeout: 10_000 }, async (t) => {
    const root = await mkdtemp(join(tmpdir(), "groundhog-proxy-"));
    t.after(() => rm(root, { recursive: true, force: true }));
    const upstream = createHttpServer(async (request, response) => {
      let body = "";
      for await (const chunk of request) body += chunk;
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify({ url: request.url, method: request.method, body }));
    });
    upstream.listen(0, "127.0.0.1");
    await once(upstream, "listening");
    t.after(() => new Promise((resolve, reject) => {
      upstream.close((error) => error ? reject(error) : resolve());
      upstream.closeAllConnections();
    }));

    const config = { rpcUrl: `http://127.0.0.1:${upstream.address().port}${endpoint}` };
    let server;
    const process = { env: { PORT: "0" } };
    // Run the template with deployment/CLI dependencies supplied by the test,
    // while using Vite's real HTTP proxy for the browser request.
    await runInNewContext(`(async () => {${template.replace(/^import .*;\n/gm, "")} })()`, {
      URL,
      console,
      process,
      readDeployment: async () => config,
      checkDeployment: async () => {},
      onShutdown: (close) => t.after(close),
      createServer: async (options) => {
        server = await createServer({ ...options, root, configFile: false, logLevel: "silent" });
        return server;
      },
    });
    assert.equal(process.exitCode, undefined, "dev server should start successfully");

    const body = JSON.stringify({ jsonrpc: "2.0", id: 1, method: "groundhog_chainInfo" });
    const response = await fetch(`http://127.0.0.1:${server.httpServer.address().port}/rpc`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body,
      signal: AbortSignal.timeout(5_000),
    });
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { url: endpoint, method: "POST", body });
  });
}
