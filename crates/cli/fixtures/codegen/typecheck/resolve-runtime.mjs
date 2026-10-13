// Resolve both client and protocol package scopes to the fixture runtime.

const runtime = new URL("./runtime.mjs", import.meta.url).href;

export async function resolve(specifier, context, next) {
  if (specifier.startsWith("@groundhogchain/") || specifier.startsWith("@thruputchain/")) {
    return { url: runtime, shortCircuit: true };
  }
  return next(specifier, context);
}
