// Module hook that resolves every @groundhogchain import to runtime.mjs.

const runtime = new URL("./runtime.mjs", import.meta.url).href;

export async function resolve(specifier, context, next) {
  if (specifier.startsWith("@groundhogchain/")) {
    return { url: runtime, shortCircuit: true };
  }
  return next(specifier, context);
}
