// Generated from recursive_type.spec.xdr - RecursiveTypeContract interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import * as __gh_protocol from "@groundhogchain/protocol";
import * as __gh_protocol_utils from "@groundhogchain/protocol/utils";
import * as __gh_contract_spec from "@groundhogchain/contract-spec";
import * as __gh_client_contract from "@groundhogchain/client/contract";

const __gh_s = __gh_contract_spec.contractSpecString;

const __gh_entrypoints = {
  RecursiveTypeContract: {
    v0_1: {
      depth: __gh_protocol.contractEntrypointID(__gh_protocol_utils.fromHex("1abed5310419683c")),
    },
  },
} as const;

export namespace demo {

  export namespace v1 {

    export interface Tree { "value": bigint; "left": __gh_codegen.demo$v1$Tree | null; "right": __gh_codegen.demo$v1$Tree | null; }
  }
}

export namespace RecursiveTypeContract {

  export namespace v0_1 {

    export interface DepthArgs {
      tree: __gh_codegen.demo$v1$Tree;
    }
    export type DepthResult = bigint;

    export type DeclaredException = never;
    export type Exception = __gh_client_contract.ContractException<DeclaredException>;
  }
}

export const spec: __gh_client_contract.ContractSpec = {
  interfaces: [
    {
      name: __gh_s("RecursiveTypeContract"),
      vers: __gh_s("0.1.0"),
      throws: [
      ],
      methods: [
        {
          name: __gh_s("depth"),
          args: [
            { name: __gh_s("tree"), ty: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("Tree") } } },
          ],
          return_type: { v: __gh_contract_spec.ContractTypeKind.U32 },
          effect: null,
        },
      ],
    },
  ],
  type_defs: [
    {
      v: __gh_contract_spec.ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("Tree") },
        fields: [
          { name: __gh_s("value"), ty: { v: __gh_contract_spec.ContractTypeKind.U32 } },
          { name: __gh_s("left"), ty: { v: __gh_contract_spec.ContractTypeKind.Option, option_inner: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("Tree") } } } },
          { name: __gh_s("right"), ty: { v: __gh_contract_spec.ContractTypeKind.Option, option_inner: { v: __gh_contract_spec.ContractTypeKind.Named, named: { crate_name: __gh_s("demo"), major: __gh_s("1"), module_path: __gh_s("demo"), declared: __gh_s("Tree") } } } },
        ],
      },
    },
  ],
  events: [
  ],
};

const __gh_codecs = __gh_client_contract.createContractCodecContext(spec.type_defs);

export const RecursiveTypeContract = {
  v0_1: {
    depth: __gh_client_contract.defineContractMethod<
      RecursiveTypeContract.v0_1.DepthArgs,
      RecursiveTypeContract.v0_1.DepthResult,
      RecursiveTypeContract.v0_1.DeclaredException
    >({
      interfaceName: "RecursiveTypeContract",
      interfaceVersion: "0.1.0",
      abiName: "depth",
      publicName: "depth",
      entrypoint: __gh_entrypoints.RecursiveTypeContract.v0_1.depth,
      effect: spec.interfaces[0]!.methods[0]!.effect,
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
      codecs: __gh_codecs,
    }),
  },
} as const;

// The types the namespaces above refer to, under names no declaration can capture.
export namespace __gh_codegen {
  export type demo$v1$Tree = demo.v1.Tree;
}
