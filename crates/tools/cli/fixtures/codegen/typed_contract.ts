// Generated from typed_contract.spec.xdr - ExampleContract interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import * as __gh_protocol from "@groundhogchain/protocol";
import * as __gh_protocol_utils from "@groundhogchain/protocol/utils";
import * as __gh_contract_spec from "@groundhogchain/contract-spec";
import * as __gh_client_contract from "@groundhogchain/client/contract";

const s = __gh_contract_spec.contractSpecString;

const entrypoints = {
  ExampleContract: {
    getValues: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("58ead8a01a28d35b")),
    update: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("b06037c26b2ec2cd")),
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
  requestId: __gh_client_contract.FixedBytes<32> | null;
}
export type ExampleContractUpdateResult = void;

export type ExampleContractDeclaredException =
  {
    kind: "declared";
    name: "ExampleError";
    selector: 0x1234n;
    value: demo.v1.ExampleError;
  };
export type ExampleContractException = __gh_client_contract.ContractException<ExampleContractDeclaredException>;

export const spec: __gh_client_contract.ContractSpec = {
  interfaces: [
    {
      name: s("ExampleContract"),
      vers: s("0.1.0"),
      throws: [
        { selector: 0x1234n, ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("ExampleError") } } },
      ],
      methods: [
        {
          name: s("get_values"),
          args: [
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.Tuple, tuple: [{ v: __gh_contract_spec.ContractTypeKind.U128 }, { v: __gh_contract_spec.ContractTypeKind.U128 }, { v: __gh_contract_spec.ContractTypeKind.U64 }] },
        },
        {
          name: s("update"),
          args: [
            { name: s("value"), ty: { v: __gh_contract_spec.ContractTypeKind.U128 } },
            { name: s("request_id"), ty: { v: __gh_contract_spec.ContractTypeKind.Option, option_inner: { v: __gh_contract_spec.ContractTypeKind.Array, array: { element: { v: __gh_contract_spec.ContractTypeKind.U8 }, len: 32n } } } },
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.Void },
        },
      ],
    },
  ],
  type_defs: [
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefUnion,
      union_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("ExampleError") },
        cases: [
          {
            name: s("InvalidValue"),
            discriminant: -1n,
            case_type: { v: __gh_contract_spec.ContractUnionCaseKind.UnionCaseVoid },
          },
          {
            name: s("UpdateFailed"),
            discriminant: 2n,
            case_type: { v: __gh_contract_spec.ContractUnionCaseKind.UnionCaseTuple, tuple: [{ v: __gh_contract_spec.ContractTypeKind.U32 }] },
          },
        ],
      },
    },
  ],
  events: [
    {
      name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("ChangedEvent") },
      signature: __gh_protocol.topic(__gh_protocol_utils.fromHex("5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a")),
      topics: [
        { name: s("actor"), ty: { v: __gh_contract_spec.ContractTypeKind.Address } },
        { name: s("request_id"), ty: { v: __gh_contract_spec.ContractTypeKind.Array, array: { element: { v: __gh_contract_spec.ContractTypeKind.U8 }, len: 32n } } },
      ],
      fields: [
        { name: s("previous_values"), ty: { v: __gh_contract_spec.ContractTypeKind.Array, array: { element: { v: __gh_contract_spec.ContractTypeKind.U128 }, len: 2n } } },
        { name: s("current_values"), ty: { v: __gh_contract_spec.ContractTypeKind.Array, array: { element: { v: __gh_contract_spec.ContractTypeKind.U128 }, len: 2n } } },
        { name: s("request_id"), ty: { v: __gh_contract_spec.ContractTypeKind.Option, option_inner: { v: __gh_contract_spec.ContractTypeKind.Array, array: { element: { v: __gh_contract_spec.ContractTypeKind.U8 }, len: 32n } } } },
      ],
    },
  ],
};

const codecs = __gh_client_contract.createContractCodecContext(spec.type_defs);

export const ExampleContract = {
  getValues: __gh_client_contract.defineContractMethod<
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
  update: __gh_client_contract.defineContractMethod<
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

    export interface ChangedEvent extends __gh_client_contract.DecodedContractEvent<
      "ChangedEvent",
      { "actor": __gh_protocol.ContractAddress; "request_id": __gh_client_contract.FixedBytes<32> },
      { "previous_values": readonly [bigint, bigint]; "current_values": readonly [bigint, bigint]; "request_id": __gh_client_contract.FixedBytes<32> | null }
    > {}

    export interface ChangedEventFilter {
      "actor"?: __gh_client_contract.ContractEventFilterValue<__gh_protocol.ContractAddress>;
      "request_id"?: __gh_client_contract.ContractEventFilterValue<__gh_client_contract.FixedBytes<32>>;
    }

    export const ChangedEvent = __gh_client_contract.defineContractEvent<
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
