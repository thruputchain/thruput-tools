// Generated from shadowed_names.spec.xdr - Shadowed interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import * as __gh_protocol from "@thruputchain/protocol";
import * as __gh_protocol_utils from "@thruputchain/protocol/utils";
import * as __gh_contract_spec from "@thruputchain/contract-spec";
import * as __gh_client_contract from "@thruputchain/client/contract";

const __gh_s = __gh_contract_spec.contractSpecString;

const __gh_entrypoints = {
  Shadowed: {
    v0_1: {
      authorize: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("820a0e00241becab")),
    },
  },
} as const;

export namespace demo {

  export namespace v1 {

    export interface Uint8Array { "bytes": globalThis.Uint8Array; }

    export interface SignedToken { "token": __gh_client_contract.SignedToken; }

    export interface Holder { "inner": __gh_codegen.other$v1$Thing; }

    export interface Record extends __gh_client_contract.DecodedContractEvent<
      "Record",
      { /** Hash of the ABI-encoded topic value; the original value cannot be recovered or directly filtered by this binding. */ "tag": globalThis.Uint8Array },
      { "note": string }
    > {}

    export type RecordFilter = globalThis.Record<string, never>;

    export namespace $globalThis {

      export interface Blob { "bytes": globalThis.Uint8Array; }
    }

    export namespace other {

      export interface Holder { "inner": __gh_codegen.other$v1$Thing; }
    }

    export namespace spec {

      export interface Pinged extends __gh_client_contract.DecodedContractEvent<
        "Pinged",
        { "tag": bigint },
        { "note": string }
      > {}

      export interface PingedFilter {
        "tag"?: __gh_client_contract.ContractEventFilterValue<bigint>;
      }
    }
  }
}

export namespace event {

  export namespace v1 {

    export interface Ping extends __gh_client_contract.DecodedContractEvent<
      "Ping",
      { "tag": bigint },
      { "note": string }
    > {}

    export interface PingFilter {
      "tag"?: __gh_client_contract.ContractEventFilterValue<bigint>;
    }
  }
}

export namespace other {

  export namespace v1 {

    export interface Thing { "n": bigint; }

    export interface demo { "holder": __gh_codegen.demo$v1$Holder; }
  }
}

export namespace Shadowed {

  export namespace v0_1 {

    export interface AuthorizeArgs {
      holder: __gh_codegen.demo$v1$Holder;
    }
    export type AuthorizeResult = __gh_client_contract.SignedToken;

    export type DeclaredException = never;
    export type Exception = __gh_client_contract.ContractException<DeclaredException>;
  }
}

export const spec: __gh_client_contract.ContractSpec = {
  interfaces: [
    {
      name: __gh_s("Shadowed"),
      vers: __gh_s("0.1.0"),
      throws: [
      ],
      methods: [
        {
          name: __gh_s("authorize"),
          args: [
            { name: __gh_s("holder"), ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("Holder") } } },
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.SignedToken, signed_token_preimage: { preimage_type: null } },
          effect: null,
        },
      ],
    },
  ],
  type_defs: [
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("Uint8Array") },
        fields: [
          { name: __gh_s("bytes"), ty: { v: __gh_contract_spec.ContractTypeKind.Bytes } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("SignedToken") },
        fields: [
          { name: __gh_s("token"), ty: { v: __gh_contract_spec.ContractTypeKind.SignedToken, signed_token_preimage: { preimage_type: null } } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("other"), major: __gh_s("1"), module_path: __gh_s("other"), declared: __gh_s("Thing") },
        fields: [
          { name: __gh_s("n"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("Holder") },
        fields: [
          { name: __gh_s("inner"), ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("other"), major: __gh_s("1"), module_path: __gh_s("other"), declared: __gh_s("Thing") } } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("other"), major: __gh_s("1"), module_path: __gh_s("other"), declared: __gh_s("demo") },
        fields: [
          { name: __gh_s("holder"), ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("Holder") } } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo::other"), declared: __gh_s("Holder") },
        fields: [
          { name: __gh_s("inner"), ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("other"), major: __gh_s("1"), module_path: __gh_s("other"), declared: __gh_s("Thing") } } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo::globalThis"), declared: __gh_s("Blob") },
        fields: [
          { name: __gh_s("bytes"), ty: { v: __gh_contract_spec.ContractTypeKind.Bytes } },
        ],
      },
    },
  ],
  events: [
    {
      name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("Record") },
      signature: __gh_protocol.topic(__gh_protocol_utils.fromHex("abababababababababababababababababababababababababababababababab")),
      topics: [
        { name: __gh_s("tag"), ty: { v: __gh_contract_spec.ContractTypeKind.String } },
      ],
      fields: [
        { name: __gh_s("note"), ty: { v: __gh_contract_spec.ContractTypeKind.String } },
      ],
    },
    {
      name: { crate_name: __gh_s("event"), major: __gh_s("1"), module_path: __gh_s("event"), declared: __gh_s("Ping") },
      signature: __gh_protocol.topic(__gh_protocol_utils.fromHex("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd")),
      topics: [
        { name: __gh_s("tag"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
      ],
      fields: [
        { name: __gh_s("note"), ty: { v: __gh_contract_spec.ContractTypeKind.String } },
      ],
    },
    {
      name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo::spec"), declared: __gh_s("Pinged") },
      signature: __gh_protocol.topic(__gh_protocol_utils.fromHex("efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef")),
      topics: [
        { name: __gh_s("tag"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
      ],
      fields: [
        { name: __gh_s("note"), ty: { v: __gh_contract_spec.ContractTypeKind.String } },
      ],
    },
  ],
};

const __gh_codecs = __gh_client_contract.createContractCodecContext(spec.type_defs);

export const Shadowed = {
  v0_1: {
    authorize: __gh_client_contract.defineContractMethod<
      Shadowed.v0_1.AuthorizeArgs,
      Shadowed.v0_1.AuthorizeResult,
      Shadowed.v0_1.DeclaredException
    >({
      interfaceName: "Shadowed",
      interfaceVersion: "0.1.0",
      abiName: "authorize",
      publicName: "authorize",
      entrypoint: __gh_entrypoints.Shadowed.v0_1.authorize,
      effect: spec.interfaces[0]!.methods[0]!.effect,
      args: [
        {
          abiName: "holder",
          publicName: "holder",
          type: spec.interfaces[0]!.methods[0]!.args[0]!.ty,
        },
      ],
      returnType: spec.interfaces[0]!.methods[0]!.return_type,
      throws: [
      ],
      codecs: __gh_codecs,
    }),
  },
} as const;

export const demo = {
  v1: {
    Record: __gh_client_contract.defineContractEvent<
      "Record",
      demo.v1.Record["topics"],
      demo.v1.Record["fields"],
      demo.v1.RecordFilter
    >({
      name: "Record",
      signature: spec.events[0]!.signature,
      topics: [
        {
          name: "tag",
          type: spec.events[0]!.topics[0]!.ty,
          representation: "hash",
        },
      ],
      fields: [
        { name: "note", type: spec.events[0]!.fields[0]!.ty },
      ],
      codecs: __gh_codecs,
    }),
    spec: {
      Pinged: __gh_client_contract.defineContractEvent<
        "Pinged",
        demo.v1.spec.Pinged["topics"],
        demo.v1.spec.Pinged["fields"],
        demo.v1.spec.PingedFilter
      >({
        name: "Pinged",
        signature: spec.events[2]!.signature,
        topics: [
          {
            name: "tag",
            type: spec.events[2]!.topics[0]!.ty,
            representation: "u64",
          },
        ],
        fields: [
          { name: "note", type: spec.events[2]!.fields[0]!.ty },
        ],
        codecs: __gh_codecs,
      }),
    },
  },
} as const;

export const event = {
  v1: {
    Ping: __gh_client_contract.defineContractEvent<
      "Ping",
      event.v1.Ping["topics"],
      event.v1.Ping["fields"],
      event.v1.PingFilter
    >({
      name: "Ping",
      signature: spec.events[1]!.signature,
      topics: [
        {
          name: "tag",
          type: spec.events[1]!.topics[0]!.ty,
          representation: "u64",
        },
      ],
      fields: [
        { name: "note", type: spec.events[1]!.fields[0]!.ty },
      ],
      codecs: __gh_codecs,
    }),
  },
} as const;

export type ShadowedEvent = demo.v1.Record | event.v1.Ping | demo.v1.spec.Pinged;

export function decodeEvent(__gh_event: { topics: globalThis.Uint8Array[]; data: globalThis.Uint8Array }): ShadowedEvent | null {
  return demo.v1.Record.decode(__gh_event) ?? event.v1.Ping.decode(__gh_event) ?? demo.v1.spec.Pinged.decode(__gh_event);
}

// The types the namespaces above refer to, under names no declaration can capture.
export namespace __gh_codegen {
  export type demo$v1$Uint8Array = demo.v1.Uint8Array;
  export type demo$v1$SignedToken = demo.v1.SignedToken;
  export type other$v1$Thing = other.v1.Thing;
  export type demo$v1$Holder = demo.v1.Holder;
  export type other$v1$demo = other.v1.demo;
  export type demo$v1$other$Holder = demo.v1.other.Holder;
  export type demo$v1$$globalThis$Blob = demo.v1.$globalThis.Blob;
}
