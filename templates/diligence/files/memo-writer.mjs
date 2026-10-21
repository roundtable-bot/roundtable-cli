// Sends work to the three bots, waits for the results, and saves the memo.
// Run `npm run dev` in another terminal first so the bots can pick up their tasks.
import { mkdir, writeFile } from "node:fs/promises";
import { Roundtable } from "@roundtable/sdk";
import { botId, identity, required } from "./lib/env.mjs";
import { renderMemo } from "./lib/memo.mjs";

const project = required("ROUNDTABLE_PROJECT");
const rt = new Roundtable({ key: identity("memo-writer").key });
const input = { prefix: "dataroom/acme/", company: "Acme Ledger, Inc." };

console.log("Dispatching to red-flags, comps and market...");
const job = await rt.dispatch(
  [
    { bot: botId("red-flags"), goal: "Find red flags in Acme Ledger's data room", input },
    { bot: botId("comps"), goal: "Find public comparables for Acme Ledger", input },
    { bot: botId("market"), goal: "Summarise market signals for Acme Ledger's category", input },
  ],
  { project },
);
const [flags, comps, market] = await job.gather({ timeoutMs: 300_000 });

const memo = renderMemo({ company: input.company, generatedOn: new Date().toISOString().slice(0, 10), flags, comps, market });

// Save to the bank, so the memo has its own version, and to ./memos for easy reading.
const saved = await rt.call("write_file", { bank: project, path: "memos/acme.md", content: memo, message: "Draft diligence memo for Acme Ledger" });
await mkdir("memos", { recursive: true });
await writeFile("memos/acme.md", memo);
console.log(`Memo saved to memos/acme.md and to ${project}:memos/acme.md (version ${saved.version.id}).`);
