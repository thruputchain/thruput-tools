// Generated from namespace_merges.spec.xdr - Merged interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import * as __gh_protocol from "@groundhogchain/protocol";
import * as __gh_protocol_utils from "@groundhogchain/protocol/utils";
import * as __gh_contract_spec from "@groundhogchain/contract-spec";
import * as __gh_client_contract from "@groundhogchain/client/contract";

const s = __gh_contract_spec.contractSpecString;

export namespace demo {

  export namespace v1 {

    export interface foo { "x": bigint; }

    export type bar =
      { case: "Ok" };

    export type baz = 1n;

    export interface qux extends __gh_client_contract.DecodedContractEvent<
      "qux",
      { "tag": bigint },
      { "note": demo.v1.qux.Detail }
    > {}

    export interface quxFilter {
      "tag"?: __gh_client_contract.ContractEventFilterValue<bigint>;
    }

    export namespace bar {

      export type Kind = 1n;
    }

    export namespace baz {

      export interface Inner { "x": bigint; }
    }

    export namespace foo {

      export interface Bar { "x": bigint; }
    }

    export namespace qux {

      export interface Detail { "x": bigint; }
    }
  }
}

export type MergedDeclaredException = never;
export type MergedException = __gh_client_contract.ContractException<MergedDeclaredException>;

export const spec: __gh_client_contract.ContractSpec = {
  interfaces: [
    {
      name: s("Merged"),
      vers: s("0.1.0"),
      throws: [
      ],
      methods: [
      ],
    },
  ],
  type_defs: [
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("foo") },
        fields: [
          { name: s("x"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo::foo"), declared: s("Bar") },
        fields: [
          { name: s("x"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefUnion,
      union_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("bar") },
        cases: [
          {
            name: s("Ok"),
            discriminant: 0n,
            case_type: { v: __gh_contract_spec.ContractUnionCaseKind.UnionCaseVoid },
          },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefEnum,
      enum_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo::bar"), declared: s("Kind") },
        variants: [
          { name: s("A"), value: 1n },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefEnum,
      enum_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("baz") },
        variants: [
          { name: s("A"), value: 1n },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo::baz"), declared: s("Inner") },
        fields: [
          { name: s("x"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo::qux"), declared: s("Detail") },
        fields: [
          { name: s("x"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
        ],
      },
    },
  ],
  events: [
    {
      name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("qux") },
      signature: __gh_protocol.topic(__gh_protocol_utils.fromHex("abababababababababababababababababababababababababababababababab")),
      topics: [
        { name: s("tag"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
      ],
      fields: [
        { name: s("note"), ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: s("demo"), major: s("1"), module_path: s("demo::qux"), declared: s("Detail") } } },
      ],
    },
  ],
};

const codecs = __gh_client_contract.createContractCodecContext(spec.type_defs);

export const Merged = {
} as const;

export const demo = {
  v1: {
    bar: {
      Kind: {
        "A": 1n,
      },
    },
    baz: {
      "A": 1n,
    },
    qux: __gh_client_contract.defineContractEvent<
      "qux",
      demo.v1.qux["topics"],
      demo.v1.qux["fields"],
      demo.v1.quxFilter
    >({
      name: "qux",
      signature: spec.events[0]!.signature,
      topics: [
        {
          name: "tag",
          type: spec.events[0]!.topics[0]!.ty,
          representation: "u64",
        },
      ],
      fields: [
        { name: "note", type: spec.events[0]!.fields[0]!.ty },
      ],
      codecs,
    }),
  },
} as const;

export type MergedEvent = demo.v1.qux;

export function decodeEvent(event: { topics: globalThis.Uint8Array[]; data: globalThis.Uint8Array }): MergedEvent | null {
  return demo.v1.qux.decode(event);
}
