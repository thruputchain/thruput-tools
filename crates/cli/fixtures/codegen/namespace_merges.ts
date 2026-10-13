// Generated from namespace_merges.spec.xdr - Merged interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import * as __gh_protocol from "@thruputchain/protocol";
import * as __gh_protocol_utils from "@thruputchain/protocol/utils";
import * as __gh_contract_spec from "@thruputchain/contract-spec";
import * as __gh_client_contract from "@thruputchain/client/contract";

const __gh_s = __gh_contract_spec.contractSpecString;

export namespace demo {

  export namespace v1 {

    export interface foo { "x": bigint; }

    export type bar =
      { case: "Ok" };

    export type baz = 1n;

    export interface qux extends __gh_client_contract.DecodedContractEvent<
      "qux",
      { "tag": bigint },
      { "note": __gh_codegen.demo$v1$qux$Detail }
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

export namespace Merged {

  export namespace v0_1 {

    export type DeclaredException = never;
    export type Exception = __gh_client_contract.ContractException<DeclaredException>;
  }
}

export const spec: __gh_client_contract.ContractSpec = {
  interfaces: [
    {
      name: __gh_s("Merged"),
      vers: __gh_s("0.1.0"),
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
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("foo") },
        fields: [
          { name: __gh_s("x"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo::foo"), declared: __gh_s("Bar") },
        fields: [
          { name: __gh_s("x"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefUnion,
      union_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("bar") },
        cases: [
          {
            name: __gh_s("Ok"),
            discriminant: 0n,
            case_type: { v: __gh_contract_spec.ContractUnionCaseKind.UnionCaseVoid },
          },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefEnum,
      enum_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo::bar"), declared: __gh_s("Kind") },
        variants: [
          { name: __gh_s("A"), value: 1n },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefEnum,
      enum_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("baz") },
        variants: [
          { name: __gh_s("A"), value: 1n },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo::baz"), declared: __gh_s("Inner") },
        fields: [
          { name: __gh_s("x"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo::qux"), declared: __gh_s("Detail") },
        fields: [
          { name: __gh_s("x"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
        ],
      },
    },
  ],
  events: [
    {
      name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("qux") },
      signature: __gh_protocol.topic(__gh_protocol_utils.fromHex("abababababababababababababababababababababababababababababababab")),
      topics: [
        { name: __gh_s("tag"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
      ],
      fields: [
        { name: __gh_s("note"), ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo::qux"), declared: __gh_s("Detail") } } },
      ],
    },
  ],
};

const __gh_codecs = __gh_client_contract.createContractCodecContext(spec.type_defs);

export const Merged = {
  v0_1: {
  },
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
      codecs: __gh_codecs,
    }),
  },
} as const;

export type MergedEvent = demo.v1.qux;

export function decodeEvent(__gh_event: { topics: globalThis.Uint8Array[]; data: globalThis.Uint8Array }): MergedEvent | null {
  return demo.v1.qux.decode(__gh_event);
}

// The types the namespaces above refer to, under names no declaration can capture.
export namespace __gh_codegen {
  export type demo$v1$foo = demo.v1.foo;
  export type demo$v1$foo$Bar = demo.v1.foo.Bar;
  export type demo$v1$bar = demo.v1.bar;
  export type demo$v1$bar$Kind = demo.v1.bar.Kind;
  export type demo$v1$baz = demo.v1.baz;
  export type demo$v1$baz$Inner = demo.v1.baz.Inner;
  export type demo$v1$qux$Detail = demo.v1.qux.Detail;
}
