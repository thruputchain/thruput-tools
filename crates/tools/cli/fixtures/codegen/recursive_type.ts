// Generated from recursive_type.spec.xdr - RecursiveTypeContract interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import * as __gh_protocol from "@groundhogchain/protocol";
import * as __gh_protocol_utils from "@groundhogchain/protocol/utils";
import * as __gh_contract_spec from "@groundhogchain/contract-spec";
import * as __gh_client_contract from "@groundhogchain/client/contract";

const s = __gh_contract_spec.contractSpecString;

const entrypoints = {
  RecursiveTypeContract: {
    depth: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("1abed5310419683c")),
  },
} as const;

export namespace demo {

  export namespace v1 {

    export interface Tree { "value": bigint; "left": demo.v1.Tree | null; "right": demo.v1.Tree | null; }
  }
}

export interface RecursiveTypeContractDepthArgs {
  tree: demo.v1.Tree;
}
export type RecursiveTypeContractDepthResult = bigint;

export type RecursiveTypeContractDeclaredException = never;
export type RecursiveTypeContractException = __gh_client_contract.ContractException<RecursiveTypeContractDeclaredException>;

export const spec: __gh_client_contract.ContractSpec = {
  interfaces: [
    {
      name: s("RecursiveTypeContract"),
      vers: s("0.1.0"),
      throws: [
      ],
      methods: [
        {
          name: s("depth"),
          args: [
            { name: s("tree"), ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("Tree") } } },
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.U32 },
        },
      ],
    },
  ],
  type_defs: [
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("Tree") },
        fields: [
          { name: s("value"), ty: { v: __gh_contract_spec.ContractTypeKind.U32 } },
          { name: s("left"), ty: { v: __gh_contract_spec.ContractTypeKind.Option, option_inner: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("Tree") } } } },
          { name: s("right"), ty: { v: __gh_contract_spec.ContractTypeKind.Option, option_inner: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("Tree") } } } },
        ],
      },
    },
  ],
  events: [
  ],
};

const codecs = __gh_client_contract.createContractCodecContext(spec.type_defs);

export const RecursiveTypeContract = {
  depth: __gh_client_contract.defineContractMethod<
    RecursiveTypeContractDepthArgs,
    RecursiveTypeContractDepthResult,
    RecursiveTypeContractDeclaredException
  >({
    interfaceName: "RecursiveTypeContract",
    interfaceVersion: "0.1.0",
    abiName: "depth",
    publicName: "depth",
    entrypoint: entrypoints.RecursiveTypeContract.depth,
    args: [
      {
        abiName: "tree",
        publicName: "tree",
        type: spec.interfaces[0]!.methods[0]!.args[0]!.ty,
      },
    ],
    returnType: spec.interfaces[0]!.methods[0]!.return_type,
    throws: [
    ],
    codecs,
  }),
} as const;
