// Assignments a consumer of the generated bindings would write. A binding can
// compile while one of its types resolves to the wrong declaration; these fail
// to type-check when it does.

import type { ContractMethodEffect, SignedToken } from "@groundhogchain/client/contract";
import { ExampleContract } from "../typed_contract.js";
import type { ContractAddress } from "@groundhogchain/protocol";
import type { ERC20 } from "../erc20.js";
import {
  $default,
  $spec,
  Token,
  demo as escaped,
} from "../escaped_names.js";
import type { demo as merged } from "../namespace_merges.js";
import type { demo, event, other } from "../shadowed_names.js";

// Spec types named after what the bindings reach for still reach the originals.
export const bytes: demo.v1.Uint8Array = { bytes: new Uint8Array() };
export const token: demo.v1.SignedToken = { token: {} as SignedToken };
export const thing: other.v1.Thing = { n: 1n };
export const holder: demo.v1.Holder = { inner: thing };
export const named: other.v1.demo = { holder };
export const recordFilter: demo.v1.RecordFilter = {};
export const record: demo.v1.Record = {
  name: "Record",
  topics: { tag: new Uint8Array() },
  fields: { note: "" },
};
// So do references from inside modules named after a crate or the global.
export const nestedHolder: demo.v1.other.Holder = { inner: thing };
export const blob: demo.v1.$globalThis.Blob = { bytes: new Uint8Array() };
export const ping: event.v1.Ping = {
  name: "Ping",
  topics: { tag: 1n },
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

// An interface's types sit under its name and major.
export const transfer: ERC20.v0_1.TransferArgs = {
  to: {} as ContractAddress,
  amount: 1n,
};
export const declared: ERC20.v0_1.DeclaredException["value"] = 100n;

// Escaped names: one path names the type and the value.
export const frozen: escaped.v1.$default.Kind = escaped.v1.$default.Kind.Frozen;
export const escapedStruct: escaped.v1.$string = { kind: 1n, thing: { x: 1n } };
export const specThing: $spec.v1.Thing = { x: 1n };
export const got: $default.v0_1.GetResult = frozen;
export const minted: typeof $spec.v1.Minted.signature = new Uint8Array();

// One interface at two majors keeps both.
export const oldBalance: Token.v0_4.BalanceResult = 1n;
export const newBalance: Token.v1.BalanceArgs = { of: {} as ContractAddress };
export const methods = [Token.v0_4.balance, Token.v1.balance, $default.v0_1.get] as const;

export const effects: readonly (ContractMethodEffect | null)[] = [
  ExampleContract.v0_1.calculate.effect,
  ExampleContract.v0_1.getValues.effect,
  ExampleContract.v0_1.update.effect,
  ExampleContract.v0_1.undeclared.effect,
];
