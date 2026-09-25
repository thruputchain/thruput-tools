// Generated from typed_contract.spec.xdr - ExampleContract interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import type { ContractAddress } from "@groundhogchain/protocol";
import { contractEntrypointID, topic } from "@groundhogchain/protocol";
import { fromHex } from "@groundhogchain/protocol/utils";
import { ContractTypeKind, ContractTypeDefKind, contractSpecString, ContractUnionCaseKind } from "@groundhogchain/contract-spec";
import { type ContractSpec, createContractCodecContext, defineContractMethod, type ContractException, type FixedBytes, defineContractEvent, type DecodedContractEvent, type ContractEventFilterValue } from "@groundhogchain/client/contract";

const s = contractSpecString;

const entrypoints = {
  ExampleContract: {
    getValues: contractEntrypointID(fromHex("58ead8a01a28d35b")),
    update: contractEntrypointID(fromHex("b06037c26b2ec2cd")),
  },
} as const;

export namespace demo {

  export namespace v1 {

    export type ExampleError =
      { case: "InvalidValue" }
      | { case: "UpdateFailed"; value: readonly [bigint] };
  }
}

export type ExampleContractGetValuesArgs = void;
export type ExampleContractGetValuesResult = readonly [bigint, bigint, bigint];

export interface ExampleContractUpdateArgs {
  value: bigint;
  requestId: FixedBytes<32> | null;
}
export type ExampleContractUpdateResult = void;

export type ExampleContractDeclaredException =
  {
    kind: "declared";
    name: "ExampleError";
    selector: 0x1234n;
    value: demo.v1.ExampleError;
  };
export type ExampleContractException = ContractException<ExampleContractDeclaredException>;

export const spec: ContractSpec = {
  interfaces: [
    {
      name: s("ExampleContract"),
      vers: s("0.1.0"),
      throws: [
        { selector: 0x1234n, ty: { v: ContractTypeKind.Named, named: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("ExampleError") } } },
      ],
      methods: [
        {
          name: s("get_values"),
          args: [
          ],
          return_type: { v: ContractTypeKind.Tuple, tuple: [{ v: ContractTypeKind.U128 }, { v: ContractTypeKind.U128 }, { v: ContractTypeKind.U64 }] },
        },
        {
          name: s("update"),
          args: [
            { name: s("value"), ty: { v: ContractTypeKind.U128 } },
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
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("ExampleError") },
        cases: [
          {
            name: s("InvalidValue"),
            discriminant: -1n,
            case_type: { v: ContractUnionCaseKind.UnionCaseVoid },
          },
          {
            name: s("UpdateFailed"),
            discriminant: 2n,
            case_type: { v: ContractUnionCaseKind.UnionCaseTuple, tuple: [{ v: ContractTypeKind.U32 }] },
          },
        ],
      },
    },
  ],
  events: [
    {
      name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("ChangedEvent") },
      signature: topic(fromHex("5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a")),
      topics: [
        { name: s("actor"), ty: { v: ContractTypeKind.Address } },
        { name: s("request_id"), ty: { v: ContractTypeKind.Array, array: { element: { v: ContractTypeKind.U8 }, len: 32n } } },
      ],
      fields: [
        { name: s("previous_values"), ty: { v: ContractTypeKind.Array, array: { element: { v: ContractTypeKind.U128 }, len: 2n } } },
        { name: s("current_values"), ty: { v: ContractTypeKind.Array, array: { element: { v: ContractTypeKind.U128 }, len: 2n } } },
        { name: s("request_id"), ty: { v: ContractTypeKind.Option, option_inner: { v: ContractTypeKind.Array, array: { element: { v: ContractTypeKind.U8 }, len: 32n } } } },
      ],
    },
  ],
};

const codecs = createContractCodecContext(spec.type_defs);

export const ExampleContract = {
  getValues: defineContractMethod<
    ExampleContractGetValuesArgs,
    ExampleContractGetValuesResult,
    ExampleContractDeclaredException
  >({
    interfaceName: "ExampleContract",
    interfaceVersion: "0.1.0",
    abiName: "get_values",
    publicName: "getValues",
    entrypoint: entrypoints.ExampleContract.getValues,
    args: [
    ],
    returnType: spec.interfaces[0]!.methods[0]!.return_type,
    throws: [
      {
        selector: 0x1234n,
        name: "ExampleError",
        type: spec.interfaces[0]!.throws[0]!.ty,
      },
    ],
    codecs,
  }),
  update: defineContractMethod<
    ExampleContractUpdateArgs,
    ExampleContractUpdateResult,
    ExampleContractDeclaredException
  >({
    interfaceName: "ExampleContract",
    interfaceVersion: "0.1.0",
    abiName: "update",
    publicName: "update",
    entrypoint: entrypoints.ExampleContract.update,
    args: [
      {
        abiName: "value",
        publicName: "value",
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
        name: "ExampleError",
        type: spec.interfaces[0]!.throws[0]!.ty,
      },
    ],
    codecs,
  }),
} as const;

export namespace demo {

  export namespace v1 {

    export interface ChangedEvent extends DecodedContractEvent<
      "ChangedEvent",
      { "actor": ContractAddress; "request_id": FixedBytes<32> },
      { "previous_values": readonly [bigint, bigint]; "current_values": readonly [bigint, bigint]; "request_id": FixedBytes<32> | null }
    > {}

    export interface ChangedEventFilter {
      "actor"?: ContractEventFilterValue<ContractAddress>;
      "request_id"?: ContractEventFilterValue<FixedBytes<32>>;
    }

    export const ChangedEvent = defineContractEvent<
      "ChangedEvent",
      ChangedEvent["topics"],
      ChangedEvent["fields"],
      ChangedEventFilter
    >({
      name: "ChangedEvent",
      signature: spec.events[0]!.signature,
      topics: [
        {
          name: "actor",
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
        { name: "previous_values", type: spec.events[0]!.fields[0]!.ty },
        { name: "current_values", type: spec.events[0]!.fields[1]!.ty },
        { name: "request_id", type: spec.events[0]!.fields[2]!.ty },
      ],
      codecs,
    });
  }
}

export type ExampleContractEvent = demo.v1.ChangedEvent;

export function decodeEvent(event: { topics: Uint8Array[]; data: Uint8Array }): ExampleContractEvent | null {
  return demo.v1.ChangedEvent.decode(event);
}
