// Generated from erc20.wasm - ERC20 interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import type { ContractAddress } from "@groundhogchain/protocol";
import { contractEntrypointID } from "@groundhogchain/protocol";
import { fromHex } from "@groundhogchain/protocol/utils";
import { ContractTypeKind, ContractTypeDefKind, contractSpecString } from "@groundhogchain/contract-spec";
import { type ContractSpec, createContractCodecContext, defineContractMethod, type ContractAbort } from "@groundhogchain/client/contract";

const s = contractSpecString;

const entrypoints = {
  ERC20: {
    transfer: contractEntrypointID(fromHex("63285b3523f9dc3f")),
    getBalance: contractEntrypointID(fromHex("b921b642d31d7dac")),
    getBalanceOf: contractEntrypointID(fromHex("41ac95d6e06e2620")),
  },
} as const;

export const ERC20Error = { "AlreadyInitialized": 1n, "MustBeContractOwner": 2n, "TooLarge": 3n, "Internal": 100n, } as const;

export type ERC20Error = (typeof ERC20Error)[keyof typeof ERC20Error];

export interface ERC20TransferArgs {
  to: ContractAddress;
  amount: bigint;
}
export type ERC20TransferResult = void;

export type ERC20GetBalanceArgs = void;
export type ERC20GetBalanceResult = bigint;

export interface ERC20GetBalanceOfArgs {
  accountId: ContractAddress;
}
export type ERC20GetBalanceOfResult = bigint;

export type ERC20DeclaredAbort =
  {
    kind: "declared";
    exception: "ERC20Error";
    selector: 0xb19947acb2fa14ffn;
    value: ERC20Error;
  };
export type ERC20Abort = ContractAbort<ERC20DeclaredAbort>;

export const spec: ContractSpec = {
  interfaces: [
    {
      name: s("ERC20"),
      vers: s("0.1.0"),
      throws: [
        { selector: 0xb19947acb2fa14ffn, ty: { v: ContractTypeKind.Named, named: s("ERC20Error") } },
      ],
      methods: [
        {
          name: s("transfer"),
          args: [
            { name: s("to"), ty: { v: ContractTypeKind.Address } },
            { name: s("amount"), ty: { v: ContractTypeKind.U128 } },
          ],
          return_type: { v: ContractTypeKind.Void },
        },
        {
          name: s("get_balance"),
          args: [
          ],
          return_type: { v: ContractTypeKind.U128 },
        },
        {
          name: s("get_balance_of"),
          args: [
            { name: s("account_id"), ty: { v: ContractTypeKind.Address } },
          ],
          return_type: { v: ContractTypeKind.U128 },
        },
      ],
    },
  ],
  type_defs: [
    {
      v: ContractTypeDefKind.TypeDefEnum,
      enum_def: {
        name: s("ERC20Error"),
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

const codecs = createContractCodecContext(spec.type_defs);

export const ERC20 = {
  transfer: defineContractMethod<
    ERC20TransferArgs,
    ERC20TransferResult,
    ERC20DeclaredAbort
  >({
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
        exception: "ERC20Error",
        type: spec.interfaces[0]!.throws[0]!.ty,
      },
    ],
    codecs,
  }),
  getBalance: defineContractMethod<
    ERC20GetBalanceArgs,
    ERC20GetBalanceResult,
    ERC20DeclaredAbort
  >({
    entrypoint: entrypoints.ERC20.getBalance,
    args: [
    ],
    returnType: spec.interfaces[0]!.methods[1]!.return_type,
    throws: [
      {
        selector: 0xb19947acb2fa14ffn,
        exception: "ERC20Error",
        type: spec.interfaces[0]!.throws[0]!.ty,
      },
    ],
    codecs,
  }),
  getBalanceOf: defineContractMethod<
    ERC20GetBalanceOfArgs,
    ERC20GetBalanceOfResult,
    ERC20DeclaredAbort
  >({
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
        exception: "ERC20Error",
        type: spec.interfaces[0]!.throws[0]!.ty,
      },
    ],
    codecs,
  }),
} as const;
