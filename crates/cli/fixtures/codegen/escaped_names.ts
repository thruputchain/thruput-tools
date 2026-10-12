// Generated from escaped_names.spec.xdr - default interface v0.1.0
// Generated from escaped_names.spec.xdr - Token interface v0.4.0
// Generated from escaped_names.spec.xdr - Token interface v1.2.0
// DO NOT EDIT. Re-run codegen to update.

import * as __gh_protocol from "@thruputchain/protocol";
import * as __gh_protocol_utils from "@thruputchain/protocol/utils";
import * as __gh_contract_spec from "@thruputchain/contract-spec";
import * as __gh_client_contract from "@thruputchain/client/contract";

const __gh_s = __gh_contract_spec.contractSpecString;

const __gh_entrypoints = {
  $default: {
    v0_1: {
      get: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("4815bf2b153a31a6")),
    },
  },
  Token: {
    v0_4: {
      balance: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("366befc8696ec826")),
    },
    v1: {
      balance: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("beaafc1d41164f48")),
    },
  },
} as const;

export namespace $spec {

  export namespace v1 {

    export interface Thing { "x": bigint; }

    export interface Minted extends __gh_client_contract.DecodedContractEvent<
      "Minted",
      { "tag": bigint },
      { "note": string }
    > {}

    export interface MintedFilter {
      "tag"?: __gh_client_contract.ContractEventFilterValue<bigint>;
    }
  }
}

export namespace demo {

  export namespace v1 {

    export interface $string { "kind": __gh_codegen.demo$v1$$default$Kind; "thing": __gh_codegen.$spec$v1$Thing; }

    export namespace $default {

      export type Kind = 1n | 2n;
    }
  }
}

export namespace $default {

  export namespace v0_1 {

    export type GetArgs = void;
    export type GetResult = __gh_codegen.demo$v1$$default$Kind;

    export type DeclaredException =
      {
        kind: "declared";
        name: "Kind";
        selector: 0x1n;
        value: __gh_codegen.demo$v1$$default$Kind;
      };
    export type Exception = __gh_client_contract.ContractException<DeclaredException>;
  }
}

export namespace Token {

  export namespace v0_4 {

    export type BalanceArgs = void;
    export type BalanceResult = bigint;

    export type DeclaredException = never;
    export type Exception = __gh_client_contract.ContractException<DeclaredException>;
  }

  export namespace v1 {

    export interface BalanceArgs {
      of: __gh_protocol.ContractAddress;
    }
    export type BalanceResult = bigint;

    export type DeclaredException = never;
    export type Exception = __gh_client_contract.ContractException<DeclaredException>;
  }
}

export const spec: __gh_client_contract.ContractSpec = {
  interfaces: [
    {
      name: __gh_s("default"),
      vers: __gh_s("0.1.0"),
      throws: [
        { selector: 0x1n, ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo::default"), declared: __gh_s("Kind") } } },
      ],
      methods: [
        {
          name: __gh_s("get"),
          args: [
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo::default"), declared: __gh_s("Kind") } },
          effect: null,
        },
      ],
    },
    {
      name: __gh_s("Token"),
      vers: __gh_s("0.4.0"),
      throws: [
      ],
      methods: [
        {
          name: __gh_s("balance"),
          args: [
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.U64 },
          effect: null,
        },
      ],
    },
    {
      name: __gh_s("Token"),
      vers: __gh_s("1.2.0"),
      throws: [
      ],
      methods: [
        {
          name: __gh_s("balance"),
          args: [
            { name: __gh_s("of"), ty: { v: __gh_contract_spec.ContractTypeKind.Address } },
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.U128 },
          effect: null,
        },
      ],
    },
  ],
  type_defs: [
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefEnum,
      enum_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo::default"), declared: __gh_s("Kind") },
        variants: [
          { name: __gh_s("Active"), value: 1n },
          { name: __gh_s("Frozen"), value: 2n },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("string") },
        fields: [
          { name: __gh_s("kind"), ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo::default"), declared: __gh_s("Kind") } } },
          { name: __gh_s("thing"), ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("spec"), major: __gh_s("1"), module_path: __gh_s("spec"), declared: __gh_s("Thing") } } },
        ],
      },
    },
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("spec"), major: __gh_s("1"), module_path: __gh_s("spec"), declared: __gh_s("Thing") },
        fields: [
          { name: __gh_s("x"), ty: { v: __gh_contract_spec.ContractTypeKind.U64 } },
        ],
      },
    },
  ],
  events: [
    {
      name: { crate_name: __gh_s("spec"), major: __gh_s("1"), module_path: __gh_s("spec"), declared: __gh_s("Minted") },
      signature: __gh_protocol.topic(__gh_protocol_utils.fromHex("abababababababababababababababababababababababababababababababab")),
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

export const $default = {
  v0_1: {
    get: __gh_client_contract.defineContractMethod<
      $default.v0_1.GetArgs,
      $default.v0_1.GetResult,
      $default.v0_1.DeclaredException
    >({
      interfaceName: "default",
      interfaceVersion: "0.1.0",
      abiName: "get",
      publicName: "get",
      entrypoint: __gh_entrypoints.$default.v0_1.get,
      effect: spec.interfaces[0]!.methods[0]!.effect,
      args: [
      ],
      returnType: spec.interfaces[0]!.methods[0]!.return_type,
      throws: [
        {
          selector: 0x1n,
          name: "Kind",
          type: spec.interfaces[0]!.throws[0]!.ty,
        },
      ],
      codecs: __gh_codecs,
    }),
  },
} as const;

export const Token = {
  v0_4: {
    balance: __gh_client_contract.defineContractMethod<
      Token.v0_4.BalanceArgs,
      Token.v0_4.BalanceResult,
      Token.v0_4.DeclaredException
    >({
      interfaceName: "Token",
      interfaceVersion: "0.4.0",
      abiName: "balance",
      publicName: "balance",
      entrypoint: __gh_entrypoints.Token.v0_4.balance,
      effect: spec.interfaces[1]!.methods[0]!.effect,
      args: [
      ],
      returnType: spec.interfaces[1]!.methods[0]!.return_type,
      throws: [
      ],
      codecs: __gh_codecs,
    }),
  },
  v1: {
    balance: __gh_client_contract.defineContractMethod<
      Token.v1.BalanceArgs,
      Token.v1.BalanceResult,
      Token.v1.DeclaredException
    >({
      interfaceName: "Token",
      interfaceVersion: "1.2.0",
      abiName: "balance",
      publicName: "balance",
      entrypoint: __gh_entrypoints.Token.v1.balance,
      effect: spec.interfaces[2]!.methods[0]!.effect,
      args: [
        {
          abiName: "of",
          publicName: "of",
          type: spec.interfaces[2]!.methods[0]!.args[0]!.ty,
        },
      ],
      returnType: spec.interfaces[2]!.methods[0]!.return_type,
      throws: [
      ],
      codecs: __gh_codecs,
    }),
  },
} as const;

export const $spec = {
  v1: {
    Minted: __gh_client_contract.defineContractEvent<
      "Minted",
      $spec.v1.Minted["topics"],
      $spec.v1.Minted["fields"],
      $spec.v1.MintedFilter
    >({
      name: "Minted",
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

export const demo = {
  v1: {
    $default: {
      Kind: {
        "Active": 1n,
        "Frozen": 2n,
      },
    },
  },
} as const;

export type defaultEvent = $spec.v1.Minted;

export function decodeEvent(__gh_event: { topics: globalThis.Uint8Array[]; data: globalThis.Uint8Array }): defaultEvent | null {
  return $spec.v1.Minted.decode(__gh_event);
}

// The types the namespaces above refer to, under names no declaration can capture.
export namespace __gh_codegen {
  export type demo$v1$$default$Kind = demo.v1.$default.Kind;
  export type demo$v1$$string = demo.v1.$string;
  export type $spec$v1$Thing = $spec.v1.Thing;
}
