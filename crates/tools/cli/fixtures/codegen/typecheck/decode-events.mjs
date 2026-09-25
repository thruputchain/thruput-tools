// Loads each compiled binding in the given directory against runtime.mjs and
// runs its decodeEvent on every event its spec declares. Type-checking cannot
// see a binding that compiles but fails when it runs.
//
// Usage: node decode-events.mjs <directory of compiled bindings>

import { readdirSync } from "node:fs";
import { register } from "node:module";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

register("./resolve-runtime.mjs", import.meta.url);

const dir = process.argv[2];
let decoded = 0;
for (const file of readdirSync(dir).filter((name) => name.endsWith(".js")).sort()) {
  const binding = await import(pathToFileURL(join(dir, file)).href);
  if (typeof binding.decodeEvent !== "function") continue;
  for (const event of binding.spec.events) {
    const result = binding.decodeEvent({ topics: [event.signature], data: new Uint8Array() });
    if (result?.name !== event.name.declared) {
      throw new Error(`${file}: decodeEvent did not dispatch ${event.name.declared}`);
    }
    decoded += 1;
  }
  console.log(`${file}: decoded ${binding.spec.events.length} events`);
}
if (decoded === 0) {
  throw new Error(`no binding in ${dir} decoded an event`);
}
