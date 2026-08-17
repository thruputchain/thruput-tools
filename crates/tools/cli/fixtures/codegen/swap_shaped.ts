// Generated from swap_pair.spec.xdr - SwapPair interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import type { ContractAddress } from "@groundhogchain/protocol";
import { contractEntrypointID, topic } from "@groundhogchain/protocol";
import { fromHex } from "@groundhogchain/protocol/utils";
import { ContractTypeKind, ContractTypeDefKind, contractSpecString, ContractUnionCaseKind } from "@groundhogchain/contract-spec";
import { type ContractSpec, createContractCodecContext, defineContractMethod, type ContractAbort, type FixedBytes, defineContractEvent, type DecodedContractEvent, type ContractEventFilterValue } from "@groundhogchain/client/contract";

const s = contractSpecString;

const entrypoints = {
  SwapPair: {
    getReserves: contractEntrypointID(fromHex("2e3ade09b6853122")),
    swap: contractEntrypointID(fromHex("24b9993b2cc41cbb")),
  },
} as const;

export type SwapError =
  { case: "InvalidAmount" }
  | { case: "TransferFailed"; value: readonly [bigint] };

export type SwapPairGetReservesArgs = void;
export type SwapPairGetReservesResult = readonly [bigint, bigint, bigint];

export interface SwapPairSwapArgs {
  amountOut: bigint;
  requestId: FixedBytes<32> | null;
}
export type SwapPairSwapResult = void;

export type SwapPairDeclaredAbort =
  {
    kind: "declared";
    exception: "SwapError";
    selector: 0x1234n;
    value: SwapError;
  };
export type SwapPairAbort = ContractAbort<SwapPairDeclaredAbort>;

export const spec: ContractSpec = {
  interfaces: [
    {
      name: s("SwapPair"),
      vers: s("0.1.0"),
      throws: [
        { selector: 0x1234n, ty: { v: ContractTypeKind.Named, named: s("SwapError") } },
      ],
      methods: [
        {
          name: s("get_reserves"),
          args: [
          ],
          return_type: { v: ContractTypeKind.Tuple, tuple: [{ v: ContractTypeKind.U128 }, { v: ContractTypeKind.U128 }, { v: ContractTypeKind.U64 }] },
        },
        {
          name: s("swap"),
          args: [
            { name: s("amount_out"), ty: { v: ContractTypeKind.U128 } },
            { name: s("request_id"), ty: { v: ContractTypeKind.Option, option_inner: { v: ContractTypeKind.Array, array: { element: { v: ContractTypeKind.U8 }, len: 32n } } } },
          ],
          return_type: { v: ContractTypeKind.Void },
        },
      ],
    },
  ],
  type_defs: [
    {
      v: ContractTypeDefKind.TypeDefUnion,
      union_def: {
        name: s("SwapError"),
        cases: [
          {
            name: s("InvalidAmount"),
            discriminant: -1n,
            case_type: { v: ContractUnionCaseKind.UnionCaseVoid },
          },
          {
            name: s("TransferFailed"),
            discriminant: 2n,
            case_type: { v: ContractUnionCaseKind.UnionCaseTuple, tuple: [{ v: ContractTypeKind.U32 }] },
          },
        ],
      },
    },
  ],
  events: [
    {
      name: s("SwapEvent"),
      signature: topic(fromHex("5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a")),
      topics: [
        { name: s("sender"), ty: { v: ContractTypeKind.Address } },
        { name: s("request_id"), ty: { v: ContractTypeKind.Array, array: { element: { v: ContractTypeKind.U8 }, len: 32n } } },
      ],
      fields: [
        { name: s("amounts_in"), ty: { v: ContractTypeKind.Array, array: { element: { v: ContractTypeKind.U128 }, len: 2n } } },
        { name: s("amounts_out"), ty: { v: ContractTypeKind.Array, array: { element: { v: ContractTypeKind.U128 }, len: 2n } } },
        { name: s("request_id"), ty: { v: ContractTypeKind.Option, option_inner: { v: ContractTypeKind.Array, array: { element: { v: ContractTypeKind.U8 }, len: 32n } } } },
      ],
    },
  ],
};

const codecs = createContractCodecContext(spec.type_defs);

export const SwapPair = {
  getReserves: defineContractMethod<
    SwapPairGetReservesArgs,
    SwapPairGetReservesResult,
    SwapPairDeclaredAbort
  >({
    entrypoint: entrypoints.SwapPair.getReserves,
    args: [
    ],
    returnType: spec.interfaces[0]!.methods[0]!.return_type,
    throws: [
      {
        selector: 0x1234n,
        exception: "SwapError",
        type: spec.interfaces[0]!.throws[0]!.ty,
      },
    ],
    codecs,
  }),
  swap: defineContractMethod<
    SwapPairSwapArgs,
    SwapPairSwapResult,
    SwapPairDeclaredAbort
  >({
    entrypoint: entrypoints.SwapPair.swap,
    args: [
      {
        abiName: "amount_out",
        publicName: "amountOut",
        type: spec.interfaces[0]!.methods[1]!.args[0]!.ty,
      },
      {
        abiName: "request_id",
        publicName: "requestId",
        type: spec.interfaces[0]!.methods[1]!.args[1]!.ty,
      },
    ],
    returnType: spec.interfaces[0]!.methods[1]!.return_type,
    throws: [
      {
        selector: 0x1234n,
        exception: "SwapError",
        type: spec.interfaces[0]!.throws[0]!.ty,
      },
    ],
    codecs,
  }),
} as const;

export interface SwapEvent extends DecodedContractEvent<
  "SwapEvent",
  { "sender": ContractAddress; "request_id": FixedBytes<32> },
  { "amounts_in": readonly [bigint, bigint]; "amounts_out": readonly [bigint, bigint]; "request_id": FixedBytes<32> | null }
> {}

export interface SwapEventFilter {
  "sender"?: ContractEventFilterValue<ContractAddress>;
  "request_id"?: ContractEventFilterValue<FixedBytes<32>>;
}

export const SwapEvent = defineContractEvent<
  "SwapEvent",
  SwapEvent["topics"],
  SwapEvent["fields"],
  SwapEventFilter
>({
  name: "SwapEvent",
  signature: spec.events[0]!.signature,
  topics: [
    {
      name: "sender",
      type: spec.events[0]!.topics[0]!.ty,
      representation: "address",
    },
    {
      name: "request_id",
      type: spec.events[0]!.topics[1]!.ty,
      representation: "fixedBytes32",
    },
  ],
  fields: [
    { name: "amounts_in", type: spec.events[0]!.fields[0]!.ty },
    { name: "amounts_out", type: spec.events[0]!.fields[1]!.ty },
    { name: "request_id", type: spec.events[0]!.fields[2]!.ty },
  ],
  codecs,
});

export type SwapPairEvent = SwapEvent;

export function decodeEvent(event: { topics: Uint8Array[]; data: Uint8Array }): SwapPairEvent | null {
  return SwapEvent.decode(event);
}
