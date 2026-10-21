// Asks the scout bot for the top founders and saves the shortlist.
// Run `npm run dev` in another terminal first so the scout can pick up its task.
import { writeFile } from "node:fs/promises";
import { Roundtable } from "@roundtable/sdk";
import { botId, identity, required } from "./lib/env.mjs";
import { renderShortlist } from "./lib/shortlist.mjs";

const project = required("ROUNDTABLE_PROJECT");
const rt = new Roundtable({ key: identity("shortlist").key });

console.log("Asking the scout to rank the founders...");
const job = await rt.dispatch(
  [{ bot: botId("scout"), goal: "Shortlist the founders most worth meeting", input: { prefix: "sourcing/", top: 8 } }],
  { project },
);
const [scout] = await job.gather({ timeoutMs: 300_000 });
if (scout.status !== "done") throw new Error(`The scout did not finish: ${scout.result?.error ?? scout.status}`);

const shortlist = renderShortlist(scout.result);
const saved = await rt.call("write_file", { bank: project, path: "sourcing/shortlist.md", content: shortlist, message: "Founder shortlist" });
await writeFile("shortlist.md", shortlist);
console.log(`Shortlist saved to shortlist.md and to ${project}:sourcing/shortlist.md (version ${saved.version.id}).`);
