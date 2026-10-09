// Checks compiled event decoders and method effects against runtime.mjs.
//
// Usage: node decode-events.mjs <directory of compiled bindings>

import { readdirSync } from "node:fs";
import { deepStrictEqual } from "node:assert";
import { register } from "node:module";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { ContractMethodEffect } from "./runtime.mjs";

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

const { ExampleContract, spec } = await import(
  pathToFileURL(join(dir, "typed_contract.js")).href
);
const methods = ExampleContract.v0_1;
deepStrictEqual(
  [
    methods.calculate.effect,
    methods.getValues.effect,
    methods.update.effect,
    methods.undeclared.effect,
  ],
  [
    ContractMethodEffect.EffectPure,
    ContractMethodEffect.EffectReadOnly,
    ContractMethodEffect.EffectStateChanging,
    null,
  ],
  "generated method descriptors must retain declared and absent effects",
);
deepStrictEqual(
  Object.fromEntries(spec.interfaces[0].methods.map((method) => [method.name, method.effect])),
  {
    get_values: ContractMethodEffect.EffectReadOnly,
    update: ContractMethodEffect.EffectStateChanging,
    calculate: ContractMethodEffect.EffectPure,
    undeclared: null,
  },
  "generated spec must retain declared and absent effects",
);
console.log("typed_contract.js: preserved all three declared effects and an absent effect");
