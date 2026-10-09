// Types of the @groundhogchain exports the generated bindings import, standing
// in for the published packages so the type-check job needs no install. Keep
// them in step when the generator starts naming something new.

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

  export enum ContractMethodEffect {
    EffectPure = 1,
    EffectReadOnly = 2,
    EffectStateChanging = 3,
  }

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
        effect: ContractMethodEffect | null;
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
    definition: { effect: ContractMethodEffect | null; [key: string]: unknown },
  ): { readonly effect: ContractMethodEffect | null };

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
