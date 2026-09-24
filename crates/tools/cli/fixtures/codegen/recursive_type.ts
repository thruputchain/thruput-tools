// Generated from recursive_type.spec.xdr - RecursiveTypeContract interface v0.1.0
// DO NOT EDIT. Re-run codegen to update.

import { contractEntrypointID } from "@groundhogchain/protocol";
import { fromHex } from "@groundhogchain/protocol/utils";
import { ContractTypeKind, ContractTypeDefKind, contractSpecString } from "@groundhogchain/contract-spec";
import { type ContractSpec, createContractCodecContext, defineContractMethod, type ContractException } from "@groundhogchain/client/contract";

const s = contractSpecString;

const entrypoints = {
  RecursiveTypeContract: {
    depth: contractEntrypointID(fromHex("1abed5310419683c")),
  },
} as const;

export interface Tree { "value": bigint; "left": Tree | null; "right": Tree | null; }

export interface RecursiveTypeContractDepthArgs {
  tree: Tree;
}
export type RecursiveTypeContractDepthResult = bigint;

export type RecursiveTypeContractDeclaredException = never;
export type RecursiveTypeContractException = ContractException<RecursiveTypeContractDeclaredException>;

export const spec: ContractSpec = {
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
            { name: s("tree"), ty: { v: ContractTypeKind.Named, named: s("Tree") } },
          ],
          return_type: { v: ContractTypeKind.U32 },
        },
      ],
    },
  ],
  type_defs: [
    {
      v: ContractTypeDefKind.TypeDefStruct,
      struct_def: {
        name: { crate_name: s("demo"), major: s("1"), module_path: s("demo"), declared: s("Tree") },
        fields: [
          { name: s("value"), ty: { v: ContractTypeKind.U32 } },
          { name: s("left"), ty: { v: ContractTypeKind.Option, option_inner: { v: ContractTypeKind.Named, named: s("Tree") } } },
          { name: s("right"), ty: { v: ContractTypeKind.Option, option_inner: { v: ContractTypeKind.Named, named: s("Tree") } } },
        ],
      },
    },
  ],
  events: [
  ],
};

const codecs = createContractCodecContext(spec.type_defs);

export const RecursiveTypeContract = {
  depth: defineContractMethod<
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
