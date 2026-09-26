// Generated from shadowed_names.spec.xdr - Shadowed interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import * as __gh_protocol from "@groundhogchain/protocol";
import * as __gh_protocol_utils from "@groundhogchain/protocol/utils";
import * as __gh_contract_spec from "@groundhogchain/contract-spec";
import * as __gh_client_contract from "@groundhogchain/client/contract";

type __gh_Holder_3 = demo.v1.other.Holder;
type __gh_Thing_2 = other.v1.Thing;

const s = __gh_contract_spec.contractSpecString;

const entrypoints = {
  Shadowed: {
    authorize: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("820a0e00241becab")),
  },
} as const;

export namespace demo {

  export namespace v1 {

    export interface Uint8Array { "bytes": globalThis.Uint8Array; }

    export interface SignedToken { "token": __gh_client_contract.SignedToken; }

    export namespace other {

      export interface Holder { "inner": __gh_Thing_2; }
    }
  }
}

export namespace other {

  export namespace v1 {

    export interface Thing { "n": bigint; }

    export interface demo {  }
  }
}

export interface ShadowedAuthorizeArgs {
  holder: __gh_Holder_3;
}
export type ShadowedAuthorizeResult = __gh_client_contract.SignedToken;

export type ShadowedDeclaredException = never;
export type ShadowedException = __gh_client_contract.ContractException<ShadowedDeclaredException>;

export const spec: __gh_client_contract.ContractSpec = {
  interfaces: [
    {
      name: s("Shadowed"),
      vers: s("0.1.0"),
      throws: [
      ],
      methods: [
        {
          name: s("authorize"),
          args: [
            { name: s("holder"), ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: s("demo"), major: s("1"), module_path: s("demo::other"), declared: s("Holder") } } },
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.SignedToken, signed_token_preimage: { preimage_type: null } },
        },
      ],
    },
  ],
  type_defs: [
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("Uint8Array") },
        fields: [
          { name: s("bytes"), ty: { v: __gh_contract_spec.ContractTypeKind.Bytes } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("SignedToken") },
        fields: [
          { name: s("token"), ty: { v: __gh_contract_spec.ContractTypeKind.SignedToken, signed_token_preimage: { preimage_type: null } } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("other"), major: s("1"), module_path: s("other"), declared: s("Thing") },
        fields: [
          { name: s("n"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo::other"), declared: s("Holder") },
        fields: [
          { name: s("inner"), ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: s("other"), major: s("1"), module_path: s("other"), declared: s("Thing") } } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("other"), major: s("1"), module_path: s("other"), declared: s("demo") },
        fields: [
        ],
      },
    },
  ],
  events: [
    {
      name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("Record") },
      signature: __gh_protocol.topic(__gh_protocol_utils.fromHex("abababababababababababababababababababababababababababababababab")),
      topics: [
        { name: s("tag"), ty: { v: __gh_contract_spec.ContractTypeKind.String } },
      ],
      fields: [
        { name: s("note"), ty: { v: __gh_contract_spec.ContractTypeKind.String } },
      ],
    },
    {
      name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("codecs") },
      signature: __gh_protocol.topic(__gh_protocol_utils.fromHex("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd")),
      topics: [
        { name: s("tag"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
      ],
      fields: [
        { name: s("note"), ty: { v: __gh_contract_spec.ContractTypeKind.String } },
      ],
    },
    {
      name: { crate_name: s("demo"), major: s("1"), module_path: s("demo::spec"), declared: s("Pinged") },
      signature: __gh_protocol.topic(__gh_protocol_utils.fromHex("efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef")),
      topics: [
        { name: s("tag"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
      ],
      fields: [
        { name: s("note"), ty: { v: __gh_contract_spec.ContractTypeKind.String } },
      ],
    },
  ],
};

const codecs = __gh_client_contract.createContractCodecContext(spec.type_defs);

export const Shadowed = {
  authorize: __gh_client_contract.defineContractMethod<
    ShadowedAuthorizeArgs,
    ShadowedAuthorizeResult,
    ShadowedDeclaredException
  >({
    interfaceName: "Shadowed",
    interfaceVersion: "0.1.0",
    abiName: "authorize",
    publicName: "authorize",
    entrypoint: entrypoints.Shadowed.authorize,
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
    codecs,
  }),
} as const;

const __gh_codecs = codecs;
const __gh_spec = spec;

export namespace demo {

  export namespace v1 {

    export interface Record extends __gh_client_contract.DecodedContractEvent<
      "Record",
      { /** Hash of the ABI-encoded topic value; the original value cannot be recovered or directly filtered by this binding. */ "tag": globalThis.Uint8Array },
      { "note": string }
    > {}

    export type RecordFilter = globalThis.Record<string, never>;

    export const Record = __gh_client_contract.defineContractEvent<
      "Record",
      Record["topics"],
      Record["fields"],
      RecordFilter
    >({
      name: "Record",
      signature: __gh_spec.events[0]!.signature,
      topics: [
        {
          name: "tag",
          type: __gh_spec.events[0]!.topics[0]!.ty,
          representation: "hash",
        },
      ],
      fields: [
        { name: "note", type: __gh_spec.events[0]!.fields[0]!.ty },
      ],
      codecs: __gh_codecs,
    });

    export interface codecs extends __gh_client_contract.DecodedContractEvent<
      "codecs",
      { "tag": bigint },
      { "note": string }
    > {}

    export interface codecsFilter {
      "tag"?: __gh_client_contract.ContractEventFilterValue<bigint>;
    }

    export const codecs = __gh_client_contract.defineContractEvent<
      "codecs",
      codecs["topics"],
      codecs["fields"],
      codecsFilter
    >({
      name: "codecs",
      signature: __gh_spec.events[1]!.signature,
      topics: [
        {
          name: "tag",
          type: __gh_spec.events[1]!.topics[0]!.ty,
          representation: "u64",
        },
      ],
      fields: [
        { name: "note", type: __gh_spec.events[1]!.fields[0]!.ty },
      ],
      codecs: __gh_codecs,
    });

    export namespace spec {

      export interface Pinged extends __gh_client_contract.DecodedContractEvent<
        "Pinged",
        { "tag": bigint },
        { "note": string }
      > {}

      export interface PingedFilter {
        "tag"?: __gh_client_contract.ContractEventFilterValue<bigint>;
      }

      export const Pinged = __gh_client_contract.defineContractEvent<
        "Pinged",
        Pinged["topics"],
        Pinged["fields"],
        PingedFilter
      >({
        name: "Pinged",
        signature: __gh_spec.events[2]!.signature,
        topics: [
          {
            name: "tag",
            type: __gh_spec.events[2]!.topics[0]!.ty,
            representation: "u64",
          },
        ],
        fields: [
          { name: "note", type: __gh_spec.events[2]!.fields[0]!.ty },
        ],
        codecs: __gh_codecs,
      });
    }
  }
}

export type ShadowedEvent = demo.v1.Record | demo.v1.codecs | demo.v1.spec.Pinged;

export function decodeEvent(event: { topics: Uint8Array[]; data: Uint8Array }): ShadowedEvent | null {
  return demo.v1.Record.decode(event) ?? demo.v1.codecs.decode(event) ?? demo.v1.spec.Pinged.decode(event);
}
