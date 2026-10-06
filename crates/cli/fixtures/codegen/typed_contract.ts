// Generated from typed_contract.spec.xdr - ExampleContract interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import * as __gh_protocol from "@groundhogchain/protocol";
import * as __gh_protocol_utils from "@groundhogchain/protocol/utils";
import * as __gh_contract_spec from "@groundhogchain/contract-spec";
import * as __gh_client_contract from "@groundhogchain/client/contract";

const __gh_s = __gh_contract_spec.contractSpecString;

const __gh_entrypoints = {
  ExampleContract: {
    v0_1: {
      getValues: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("58ead8a01a28d35b")),
      update: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("b06037c26b2ec2cd")),
    },
  },
} as const;

export namespace demo {

  export namespace v1 {

    export type ExampleError =
      { case: "InvalidValue" }
      | { case: "UpdateFailed"; value: readonly [bigint] };

    export interface ChangedEvent extends __gh_client_contract.DecodedContractEvent<
      "ChangedEvent",
      { "actor": __gh_protocol.ContractAddress; "request_id": __gh_client_contract.FixedBytes<32> },
      { "previous_values": readonly [bigint, bigint]; "current_values": readonly [bigint, bigint]; "request_id": __gh_client_contract.FixedBytes<32> | null }
    > {}

    export interface ChangedEventFilter {
      "actor"?: __gh_client_contract.ContractEventFilterValue<__gh_protocol.ContractAddress>;
      "request_id"?: __gh_client_contract.ContractEventFilterValue<__gh_client_contract.FixedBytes<32>>;
    }
  }
}

export namespace ExampleContract {

  export namespace v0_1 {

    export type GetValuesArgs = void;
    export type GetValuesResult = readonly [bigint, bigint, bigint];

    export interface UpdateArgs {
      value: bigint;
      requestId: __gh_client_contract.FixedBytes<32> | null;
    }
    export type UpdateResult = void;

    export type DeclaredException =
      {
        kind: "declared";
        name: "ExampleError";
        selector: 0x1234n;
        value: __gh_codegen.demo$v1$ExampleError;
      };
    export type Exception = __gh_client_contract.ContractException<DeclaredException>;
  }
}

export const spec: __gh_client_contract.ContractSpec = {
  interfaces: [
    {
      name: __gh_s("ExampleContract"),
      vers: __gh_s("0.1.0"),
      throws: [
        { selector: 0x1234n, ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("ExampleError") } } },
      ],
      methods: [
        {
          name: __gh_s("get_values"),
          args: [
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.Tuple, tuple: [{ v: __gh_contract_spec.ContractTypeKind.U128 }, { v: __gh_contract_spec.ContractTypeKind.U128 }, { v: __gh_contract_spec.ContractTypeKind.U64 }] },
          effect: null,
        },
        {
          name: __gh_s("update"),
          args: [
            { name: __gh_s("value"), ty: { v: __gh_contract_spec.ContractTypeKind.U128 } },
            { name: __gh_s("request_id"), ty: { v: __gh_contract_spec.ContractTypeKind.Option, option_inner: { v: __gh_contract_spec.ContractTypeKind.Array, array: { element: { v: __gh_contract_spec.ContractTypeKind.U8 }, len: 32n } } } },
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.Void },
          effect: null,
        },
      ],
    },
  ],
  type_defs: [
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefUnion,
      union_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("ExampleError") },
        cases: [
          {
            name: __gh_s("InvalidValue"),
            discriminant: -1n,
            case_type: { v: __gh_contract_spec.ContractUnionCaseKind.UnionCaseVoid },
          },
          {
            name: __gh_s("UpdateFailed"),
            discriminant: 2n,
            case_type: { v: __gh_contract_spec.ContractUnionCaseKind.UnionCaseTuple, tuple: [{ v: __gh_contract_spec.ContractTypeKind.U32 }] },
          },
        ],
      },
    },
  ],
  events: [
    {
      name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("ChangedEvent") },
      signature: __gh_protocol.topic(__gh_protocol_utils.fromHex("5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a")),
      topics: [
        { name: __gh_s("actor"), ty: { v: __gh_contract_spec.ContractTypeKind.Address } },
        { name: __gh_s("request_id"), ty: { v: __gh_contract_spec.ContractTypeKind.Array, array: { element: { v: __gh_contract_spec.ContractTypeKind.U8 }, len: 32n } } },
      ],
      fields: [
        { name: __gh_s("previous_values"), ty: { v: __gh_contract_spec.ContractTypeKind.Array, array: { element: { v: __gh_contract_spec.ContractTypeKind.U128 }, len: 2n } } },
        { name: __gh_s("current_values"), ty: { v: __gh_contract_spec.ContractTypeKind.Array, array: { element: { v: __gh_contract_spec.ContractTypeKind.U128 }, len: 2n } } },
        { name: __gh_s("request_id"), ty: { v: __gh_contract_spec.ContractTypeKind.Option, option_inner: { v: __gh_contract_spec.ContractTypeKind.Array, array: { element: { v: __gh_contract_spec.ContractTypeKind.U8 }, len: 32n } } } },
      ],
    },
  ],
};

const __gh_codecs = __gh_client_contract.createContractCodecContext(spec.type_defs);

export const ExampleContract = {
  v0_1: {
    getValues: __gh_client_contract.defineContractMethod<
      ExampleContract.v0_1.GetValuesArgs,
      ExampleContract.v0_1.GetValuesResult,
      ExampleContract.v0_1.DeclaredException
    >({
      interfaceName: "ExampleContract",
      interfaceVersion: "0.1.0",
      abiName: "get_values",
      publicName: "getValues",
      entrypoint: __gh_entrypoints.ExampleContract.v0_1.getValues,
      effect: spec.interfaces[0]!.methods[0]!.effect,
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
      codecs: __gh_codecs,
    }),
    update: __gh_client_contract.defineContractMethod<
      ExampleContract.v0_1.UpdateArgs,
      ExampleContract.v0_1.UpdateResult,
      ExampleContract.v0_1.DeclaredException
    >({
      interfaceName: "ExampleContract",
      interfaceVersion: "0.1.0",
      abiName: "update",
      publicName: "update",
      entrypoint: __gh_entrypoints.ExampleContract.v0_1.update,
      effect: spec.interfaces[0]!.methods[1]!.effect,
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
      codecs: __gh_codecs,
    }),
  },
} as const;

export const demo = {
  v1: {
    ChangedEvent: __gh_client_contract.defineContractEvent<
      "ChangedEvent",
      demo.v1.ChangedEvent["topics"],
      demo.v1.ChangedEvent["fields"],
      demo.v1.ChangedEventFilter
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
      codecs: __gh_codecs,
    }),
  },
} as const;

export type ExampleContractEvent = demo.v1.ChangedEvent;

export function decodeEvent(__gh_event: { topics: globalThis.Uint8Array[]; data: globalThis.Uint8Array }): ExampleContractEvent | null {
  return demo.v1.ChangedEvent.decode(__gh_event);
}

// The types the namespaces above refer to, under names no declaration can capture.
export namespace __gh_codegen {
  export type demo$v1$ExampleError = demo.v1.ExampleError;
}
