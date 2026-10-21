// Asks the watcher bot to review every portfolio company and saves a digest of early warnings.
// Run `npm run dev` in another terminal first so the watcher can pick up its task.
import { writeFile } from "node:fs/promises";
import { Roundtable } from "@roundtable/sdk";
import { renderDigest } from "./lib/digest.mjs";
import { botId, identity, required } from "./lib/env.mjs";

const project = required("ROUNDTABLE_PROJECT");
const rt = new Roundtable({ key: identity("digest").key });

console.log("Asking the watcher to review the portfolio...");
const job = await rt.dispatch(
  [{ bot: botId("watcher"), goal: "Review the latest founder updates and raise early warnings", input: { prefix: "portfolio/" } }],
  { project },
);
const [watcher] = await job.gather({ timeoutMs: 300_000 });
if (watcher.status !== "done") throw new Error(`The watcher did not finish: ${watcher.result?.error ?? watcher.status}`);

const digest = renderDigest(watcher.result);
const saved = await rt.call("write_file", { bank: project, path: "portfolio/early-warnings.md", content: digest, message: "Portfolio early warnings" });
await writeFile("early-warnings.md", digest);
console.log(`Digest saved to early-warnings.md and to ${project}:portfolio/early-warnings.md (version ${saved.version.id}).`);
