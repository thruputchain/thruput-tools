// Assignments a consumer of the generated bindings would write. A binding can
// compile while one of its types resolves to the wrong declaration; these fail
// to type-check when it does.

import type { SignedToken } from "@groundhogchain/client/contract";
import type { demo as merged } from "../namespace_merges.js";
import type { demo, other } from "../shadowed_names.js";

// Spec types named after what the bindings reach for still reach the originals.
export const bytes: demo.v1.Uint8Array = { bytes: new Uint8Array() };
export const bytesInGlobalThis: demo.v1.globalThis.Uint8Array = { bytes: new Uint8Array() };
export const token: demo.v1.SignedToken = { token: {} as SignedToken };
export const thing: other.v1.Thing = { n: 1n };
export const holder: demo.v1.other.Holder = { inner: thing };
export const recordFilter: demo.v1.RecordFilter = {};
export const record: demo.v1.Record = {
  name: "Record",
  topics: { tag: new Uint8Array() },
  fields: { note: "" },
};

// A declaration and a module of the same name both stay reachable.
export const struct: merged.v1.foo = { x: 1n };
export const structModule: merged.v1.foo.Bar = { x: 1n };
export const union: merged.v1.bar = { case: "Ok" };
export const unionModule: merged.v1.bar.Kind = 1n;
export const enumValue: merged.v1.baz = 1n;
export const enumModule: merged.v1.baz.Inner = { x: 1n };
export const eventModule: merged.v1.qux.Detail = { x: 1n };
