// Ambient declarations of the runtime surface that generated bindings import.
//
// These stand in for the published @groundhogchain packages so the type-check
// job is hermetic: no registry auth, no lockfile, no install. They mirror the
// real exports in the way that matters for checking generated code -- every
// name exists, with its real generic arity and constraints -- while leaving
// internals the generator never names as loose types.
//
// What this proves: the generated TypeScript parses, every name it writes
// resolves, and nothing it declares captures a name it also references.
// What it does not prove: that the bindings agree with the runtime's current
// implementation. client-ts's own build is what checks that.
//
// Keep in step with @groundhogchain/client/contract when the generator starts
// naming something new; an import the generator emits and this file omits
// fails the job with "has no exported member".

declare module "@groundhogchain/protocol" {
  export type ContractAddress = Uint8Array & { readonly __brand: "ContractAddress" };
  export function contractEntrypointID(bytes: Uint8Array): Uint8Array;
  export function topic(value: Uint8Array): Uint8Array;
}

declare module "@groundhogchain/protocol/utils" {
  export function fromHex(hex: string): Uint8Array;
}

declare module "@groundhogchain/contract-spec" {
  export const ContractTypeKind: Record<string, number>;
  export const ContractTypeDefKind: Record<string, number>;
  export const ContractUnionCaseKind: Record<string, number>;
  export function contractSpecString(value: string): unknown;
}

declare module "@groundhogchain/client/contract" {
  import type { ContractAddress } from "@groundhogchain/protocol";

  /** Shaped enough that the generated spec literal is checked, not just
   *  accepted: the generator indexes into it by these exact field names. */
  export interface ContractSpecId {
    crate_name: unknown;
    major: unknown;
    module_path: unknown;
    declared: unknown;
  }
  export interface ContractSpecField {
    name: unknown;
    ty: unknown;
  }
  export interface ContractSpec {
    interfaces: readonly {
      name: unknown;
      vers: unknown;
      throws: readonly { selector: bigint; ty: unknown }[];
      methods: readonly {
        name: unknown;
        args: readonly ContractSpecField[];
        return_type: unknown;
      }[];
    }[];
    type_defs: readonly unknown[];
    events: readonly {
      name: ContractSpecId;
      signature: Uint8Array;
      topics: readonly ContractSpecField[];
      fields: readonly ContractSpecField[];
    }[];
  }
  export type FixedBytes<N extends number> = Uint8Array & { readonly length: N };
  export type FixedArray<T, N extends number> = readonly T[] & { readonly length: N };
  export interface SignedToken<TPreimage = unknown> {
    readonly __preimage?: TPreimage;
  }

  export type UnknownContractException = { variant?: string; value?: unknown };
  export type ContractException<TDeclared> =
    | (TDeclared & { variant?: string })
    | UnknownContractException;

  export interface DecodedContractEvent<TName extends string, TTopics, TFields> {
    name: TName;
    topics: TTopics;
    fields: TFields;
  }

  export type ContractEventFilterValue<T> = T | { oneOf: readonly [T, ...T[]] };

  export interface ContractEvent<TName extends string, TTopics, TFields, TFilter> {
    readonly signature: Uint8Array;
    decode(event: {
      topics: Uint8Array[];
      data: Uint8Array;
    }): DecodedContractEvent<TName, TTopics, TFields> | null;
    filter(input: {
      contractAddresses: readonly ContractAddress[];
      topics?: TFilter;
    }): unknown;
  }

  export function createContractCodecContext(typeDefs: unknown): unknown;

  export function defineContractMethod<TArgs, TResult, TDeclaredException>(
    definition: unknown,
  ): unknown;

  export function defineContractEvent<
    const TName extends string,
    TTopics,
    TFields,
    TFilter,
  >(definition: {
    name: TName;
    signature: Uint8Array;
    topics: readonly unknown[];
    fields: readonly unknown[];
    codecs: unknown;
  }): ContractEvent<TName, TTopics, TFields, TFilter>;
}
