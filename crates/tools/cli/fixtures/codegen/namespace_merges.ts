// Generated from namespace_merges.spec.xdr - Merged interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import { topic } from "@groundhogchain/protocol";
import { fromHex } from "@groundhogchain/protocol/utils";
import { ContractTypeKind, ContractTypeDefKind, contractSpecString, ContractUnionCaseKind } from "@groundhogchain/contract-spec";
import { type ContractSpec, createContractCodecContext, type ContractException, defineContractEvent, type DecodedContractEvent, type ContractEventFilterValue } from "@groundhogchain/client/contract";

const s = contractSpecString;

export namespace demo {

  export namespace v1 {

    export interface foo { "x": bigint; }

    export type bar =
      { case: "Ok" };

    export const baz = { "A": 1n, } as const;

    export type baz = (typeof baz)[keyof typeof baz];

    export namespace bar {

      export const Kind = { "A": 1n, } as const;

      export type Kind = (typeof Kind)[keyof typeof Kind];
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
export type MergedException = ContractException<MergedDeclaredException>;

export const spec: ContractSpec = {
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
      v: ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("foo") },
        fields: [
          { name: s("x"), ty: { v: ContractTypeKind.U64 } },
        ],
      },
    },
    {
      v: ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo::foo"), declared: s("Bar") },
        fields: [
          { name: s("x"), ty: { v: ContractTypeKind.U64 } },
        ],
      },
    },
    {
      v: ContractTypeDefKind.TypeDefUnion,
      union_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("bar") },
        cases: [
          {
            name: s("Ok"),
            discriminant: 0n,
            case_type: { v: ContractUnionCaseKind.UnionCaseVoid },
          },
        ],
      },
    },
    {
      v: ContractTypeDefKind.TypeDefEnum,
      enum_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo::bar"), declared: s("Kind") },
        variants: [
          { name: s("A"), value: 1n },
        ],
      },
    },
    {
      v: ContractTypeDefKind.TypeDefEnum,
      enum_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("baz") },
        variants: [
          { name: s("A"), value: 1n },
        ],
      },
    },
    {
      v: ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo::baz"), declared: s("Inner") },
        fields: [
          { name: s("x"), ty: { v: ContractTypeKind.U64 } },
        ],
      },
    },
    {
      v: ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo::qux"), declared: s("Detail") },
        fields: [
          { name: s("x"), ty: { v: ContractTypeKind.U64 } },
        ],
      },
    },
  ],
  events: [
    {
      name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("qux") },
      signature: topic(fromHex("abababababababababababababababababababababababababababababababab")),
      topics: [
        { name: s("tag"), ty: { v: ContractTypeKind.U64 } },
      ],
      fields: [
        { name: s("note"), ty: { v: ContractTypeKind.Named, named: { crate_name: s("demo"), major: s("1"), module_path: s("demo::qux"), declared: s("Detail") } } },
      ],
    },
  ],
};

const codecs = createContractCodecContext(spec.type_defs);

export const Merged = {
} as const;

export namespace demo {

  export namespace v1 {

    export interface qux extends DecodedContractEvent<
      "qux",
      { "tag": bigint },
      { "note": demo.v1.qux.Detail }
    > {}

    export interface quxFilter {
      "tag"?: ContractEventFilterValue<bigint>;
    }

    export const qux = defineContractEvent<
      "qux",
      qux["topics"],
      qux["fields"],
      quxFilter
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
    });
  }
}

export type MergedEvent = demo.v1.qux;

export function decodeEvent(event: { topics: Uint8Array[]; data: Uint8Array }): MergedEvent | null {
  return demo.v1.qux.decode(event);
}
