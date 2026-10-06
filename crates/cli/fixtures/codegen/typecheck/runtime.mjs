// Stand-ins for the values the generated bindings import from the
// @groundhogchain packages: enough to load a binding and run its decodeEvent.

const kinds = new Proxy({}, { get: (_, key) => String(key) });
export const ContractTypeKind = kinds;
export const ContractTypeDefKind = kinds;
export const ContractUnionCaseKind = kinds;
export const ContractMethodEffect = {
  EffectPure: 1,
  EffectReadOnly: 2,
  EffectStateChanging: 3,
};
export const contractSpecString = (value) => value;

export const contractEntrypointID = (bytes) => bytes;
export const topic = (bytes) => bytes;
export const fromHex = (hex) =>
  Uint8Array.from(hex.match(/../g) ?? [], (byte) => Number.parseInt(byte, 16));

export const createContractCodecContext = (typeDefs) => ({ typeDefs });
export const defineContractMethod = (definition) => definition;

export function defineContractEvent(definition) {
  const { name, signature, codecs } = definition;
  if (!(signature instanceof Uint8Array) || codecs === undefined) {
    throw new Error(`event ${name} was defined without its signature or codec context`);
  }
  const matches = (bytes) =>
    bytes?.length === signature.length && bytes.every((byte, i) => byte === signature[i]);
  return {
    signature,
    decode: (event) => (matches(event.topics[0]) ? { name, topics: {}, fields: {} } : null),
  };
}
