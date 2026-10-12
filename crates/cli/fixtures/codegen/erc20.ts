// Generated from erc20.spec.xdr - ERC20 interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import * as __gh_protocol from "@thruputchain/protocol";
import * as __gh_protocol_utils from "@thruputchain/protocol/utils";
import * as __gh_contract_spec from "@thruputchain/contract-spec";
import * as __gh_client_contract from "@thruputchain/client/contract";

const __gh_s = __gh_contract_spec.contractSpecString;

const __gh_entrypoints = {
  ERC20: {
    v0_1: {
      transfer: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("63285b3523f9dc3f")),
      getBalance: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("b921b642d31d7dac")),
      getBalanceOf: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("41ac95d6e06e2620")),
    },
  },
} as const;

export namespace demo {

  export namespace v1 {

    export type ERC20Error = 1n | 2n | 3n | 100n;
  }
}

export namespace ERC20 {

  export namespace v0_1 {

    export interface TransferArgs {
      to: __gh_protocol.ContractAddress;
      amount: bigint;
    }
    export type TransferResult = void;

    export type GetBalanceArgs = void;
    export type GetBalanceResult = bigint;

    export interface GetBalanceOfArgs {
      accountId: __gh_protocol.ContractAddress;
    }
    export type GetBalanceOfResult = bigint;

    export type DeclaredException =
      {
        kind: "declared";
        name: "ERC20Error";
        selector: 0xb19947acb2fa14ffn;
        value: __gh_codegen.demo$v1$ERC20Error;
      };
    export type Exception = __gh_client_contract.ContractException<DeclaredException>;
  }
}

export const spec: __gh_client_contract.ContractSpec = {
  interfaces: [
    {
      name: __gh_s("ERC20"),
      vers: __gh_s("0.1.0"),
      throws: [
        { selector: 0xb19947acb2fa14ffn, ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("ERC20Error") } } },
      ],
      methods: [
        {
          name: __gh_s("transfer"),
          args: [
            { name: __gh_s("to"), ty: { v: __gh_contract_spec.ContractTypeKind.Address } },
            { name: __gh_s("amount"), ty: { v: __gh_contract_spec.ContractTypeKind.U128 } },
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.Void },
          effect: null,
        },
        {
          name: __gh_s("get_balance"),
          args: [
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.U128 },
          effect: null,
        },
        {
          name: __gh_s("get_balance_of"),
          args: [
            { name: __gh_s("account_id"), ty: { v: __gh_contract_spec.ContractTypeKind.Address } },
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
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("ERC20Error") },
        variants: [
          { name: __gh_s("AlreadyInitialized"), value: 1n },
          { name: __gh_s("MustBeContractOwner"), value: 2n },
          { name: __gh_s("TooLarge"), value: 3n },
          { name: __gh_s("Internal"), value: 100n },
        ],
      },
    },
  ],
  events: [
  ],
};

const __gh_codecs = __gh_client_contract.createContractCodecContext(spec.type_defs);

export const ERC20 = {
  v0_1: {
    transfer: __gh_client_contract.defineContractMethod<
      ERC20.v0_1.TransferArgs,
      ERC20.v0_1.TransferResult,
      ERC20.v0_1.DeclaredException
    >({
      interfaceName: "ERC20",
      interfaceVersion: "0.1.0",
      abiName: "transfer",
      publicName: "transfer",
      entrypoint: __gh_entrypoints.ERC20.v0_1.transfer,
      effect: spec.interfaces[0]!.methods[0]!.effect,
      args: [
        {
          abiName: "to",
          publicName: "to",
          type: spec.interfaces[0]!.methods[0]!.args[0]!.ty,
        },
        {
          abiName: "amount",
          publicName: "amount",
          type: spec.interfaces[0]!.methods[0]!.args[1]!.ty,
        },
      ],
      returnType: spec.interfaces[0]!.methods[0]!.return_type,
      throws: [
        {
          selector: 0xb19947acb2fa14ffn,
          name: "ERC20Error",
          type: spec.interfaces[0]!.throws[0]!.ty,
        },
      ],
      codecs: __gh_codecs,
    }),
    getBalance: __gh_client_contract.defineContractMethod<
      ERC20.v0_1.GetBalanceArgs,
      ERC20.v0_1.GetBalanceResult,
      ERC20.v0_1.DeclaredException
    >({
      interfaceName: "ERC20",
      interfaceVersion: "0.1.0",
      abiName: "get_balance",
      publicName: "getBalance",
      entrypoint: __gh_entrypoints.ERC20.v0_1.getBalance,
      effect: spec.interfaces[0]!.methods[1]!.effect,
      args: [
      ],
      returnType: spec.interfaces[0]!.methods[1]!.return_type,
      throws: [
        {
          selector: 0xb19947acb2fa14ffn,
          name: "ERC20Error",
          type: spec.interfaces[0]!.throws[0]!.ty,
        },
      ],
      codecs: __gh_codecs,
    }),
    getBalanceOf: __gh_client_contract.defineContractMethod<
      ERC20.v0_1.GetBalanceOfArgs,
      ERC20.v0_1.GetBalanceOfResult,
      ERC20.v0_1.DeclaredException
    >({
      interfaceName: "ERC20",
      interfaceVersion: "0.1.0",
      abiName: "get_balance_of",
      publicName: "getBalanceOf",
      entrypoint: __gh_entrypoints.ERC20.v0_1.getBalanceOf,
      effect: spec.interfaces[0]!.methods[2]!.effect,
      args: [
        {
          abiName: "account_id",
          publicName: "accountId",
          type: spec.interfaces[0]!.methods[2]!.args[0]!.ty,
        },
      ],
      returnType: spec.interfaces[0]!.methods[2]!.return_type,
      throws: [
        {
          selector: 0xb19947acb2fa14ffn,
          name: "ERC20Error",
          type: spec.interfaces[0]!.throws[0]!.ty,
        },
      ],
      codecs: __gh_codecs,
    }),
  },
} as const;

export const demo = {
  v1: {
    ERC20Error: {
      "AlreadyInitialized": 1n,
      "MustBeContractOwner": 2n,
      "TooLarge": 3n,
      "Internal": 100n,
    },
  },
} as const;

// The types the namespaces above refer to, under names no declaration can capture.
export namespace __gh_codegen {
  export type demo$v1$ERC20Error = demo.v1.ERC20Error;
}
