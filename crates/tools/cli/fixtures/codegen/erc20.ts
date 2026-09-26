// Generated from erc20.spec.xdr - ERC20 interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import * as __gh_protocol from "@groundhogchain/protocol";
import * as __gh_protocol_utils from "@groundhogchain/protocol/utils";
import * as __gh_contract_spec from "@groundhogchain/contract-spec";
import * as __gh_client_contract from "@groundhogchain/client/contract";

const s = __gh_contract_spec.contractSpecString;

const entrypoints = {
  ERC20: {
    transfer: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("63285b3523f9dc3f")),
    getBalance: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("b921b642d31d7dac")),
    getBalanceOf: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("41ac95d6e06e2620")),
  },
} as const;

export namespace demo {

  export namespace v1 {

    export const ERC20Error = { "AlreadyInitialized": 1n, "MustBeContractOwner": 2n, "TooLarge": 3n, "Internal": 100n, } as const;

    export type ERC20Error = (typeof ERC20Error)[keyof typeof ERC20Error];
  }
}

export interface ERC20TransferArgs {
  to: __gh_protocol.ContractAddress;
  amount: bigint;
}
export type ERC20TransferResult = void;

export type ERC20GetBalanceArgs = void;
export type ERC20GetBalanceResult = bigint;

export interface ERC20GetBalanceOfArgs {
  accountId: __gh_protocol.ContractAddress;
}
export type ERC20GetBalanceOfResult = bigint;

export type ERC20DeclaredException =
  {
    kind: "declared";
    name: "ERC20Error";
    selector: 0xb19947acb2fa14ffn;
    value: demo.v1.ERC20Error;
  };
export type ERC20Exception = __gh_client_contract.ContractException<ERC20DeclaredException>;

export const spec: __gh_client_contract.ContractSpec = {
  interfaces: [
    {
      name: s("ERC20"),
      vers: s("0.1.0"),
      throws: [
        { selector: 0xb19947acb2fa14ffn, ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("ERC20Error") } } },
      ],
      methods: [
        {
          name: s("transfer"),
          args: [
            { name: s("to"), ty: { v: __gh_contract_spec.ContractTypeKind.Address } },
            { name: s("amount"), ty: { v: __gh_contract_spec.ContractTypeKind.U128 } },
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.Void },
        },
        {
          name: s("get_balance"),
          args: [
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.U128 },
        },
        {
          name: s("get_balance_of"),
          args: [
            { name: s("account_id"), ty: { v: __gh_contract_spec.ContractTypeKind.Address } },
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.U128 },
        },
      ],
    },
  ],
  type_defs: [
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefEnum,
      enum_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("ERC20Error") },
        variants: [
          { name: s("AlreadyInitialized"), value: 1n },
          { name: s("MustBeContractOwner"), value: 2n },
          { name: s("TooLarge"), value: 3n },
          { name: s("Internal"), value: 100n },
        ],
      },
    },
  ],
  events: [
  ],
};

const codecs = __gh_client_contract.createContractCodecContext(spec.type_defs);

export const ERC20 = {
  transfer: __gh_client_contract.defineContractMethod<
    ERC20TransferArgs,
    ERC20TransferResult,
    ERC20DeclaredException
  >({
    interfaceName: "ERC20",
    interfaceVersion: "0.1.0",
    abiName: "transfer",
    publicName: "transfer",
    entrypoint: entrypoints.ERC20.transfer,
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
    codecs,
  }),
  getBalance: __gh_client_contract.defineContractMethod<
    ERC20GetBalanceArgs,
    ERC20GetBalanceResult,
    ERC20DeclaredException
  >({
    interfaceName: "ERC20",
    interfaceVersion: "0.1.0",
    abiName: "get_balance",
    publicName: "getBalance",
    entrypoint: entrypoints.ERC20.getBalance,
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
    codecs,
  }),
  getBalanceOf: __gh_client_contract.defineContractMethod<
    ERC20GetBalanceOfArgs,
    ERC20GetBalanceOfResult,
    ERC20DeclaredException
  >({
    interfaceName: "ERC20",
    interfaceVersion: "0.1.0",
    abiName: "get_balance_of",
    publicName: "getBalanceOf",
    entrypoint: entrypoints.ERC20.getBalanceOf,
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
    codecs,
  }),
} as const;
