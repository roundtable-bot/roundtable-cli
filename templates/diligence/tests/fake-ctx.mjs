// A stand-in for the SDK's task context, backed by a folder of files, so bots can be run
// without a server. Versions are the first 12 hex characters of the content's SHA-256.
import { createHash } from "node:crypto";

export function fakeCtx(bank, files) {
  const store = new Map(Object.entries(files));
  const source = (path) => ({ bank, path, version: createHash("sha256").update(store.get(path)).digest("hex").slice(0, 12) });
  return {
    store,
    async list(prefix) {
      return [...store.keys()].filter((p) => p.startsWith(prefix)).map((path) => ({ path, size: store.get(path).length }));
    },
    async read(path) {
      if (!store.has(path)) throw new Error(`no such file: ${path}`);
      return { content: store.get(path), source: source(path) };
    },
    async save(path, content) {
      store.set(path, content);
      return source(path);
    },
    async progress() {},
  };
}
