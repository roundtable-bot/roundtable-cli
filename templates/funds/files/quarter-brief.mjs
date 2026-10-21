// Asks the fund-reader bot about every fund and saves a brief of what moved since last quarter.
// Run `npm run dev` in another terminal first so the fund-reader can pick up its tasks.
import { writeFile } from "node:fs/promises";
import { Roundtable } from "@roundtable/sdk";
import { fundSlugs, renderBrief } from "./lib/brief.mjs";
import { botId, identity, required } from "./lib/env.mjs";

const project = required("ROUNDTABLE_PROJECT");
const rt = new Roundtable({ key: identity("quarter-brief").key });

// One task per fund, listed in funds/lp.md. They run in order on the one fund-reader bot.
const { content } = await rt.call("read_file", { bank: project, path: "funds/lp.md" });
const slugs = fundSlugs(content);
console.log(`Asking the fund-reader about ${slugs.join(" and ")}...`);
const job = await rt.dispatch(
  slugs.map((fund) => ({ bot: botId("fund-reader"), goal: `Explain what moved in ${fund} since last quarter`, input: { fund } })),
  { project },
);
const results = await job.gather({ timeoutMs: 300_000 });
const failed = results.filter((r) => r.status !== "done");
if (failed.length) throw new Error(`The fund-reader did not finish: ${failed.map((r) => r.result?.error ?? r.status).join("; ")}`);

const brief = renderBrief(results.map((r) => r.result));
const saved = await rt.call("write_file", { bank: project, path: "funds/briefs/latest.md", content: brief, message: "What moved since last quarter" });
await writeFile("quarter-brief.md", brief);
console.log(`Brief saved to quarter-brief.md and to ${project}:funds/briefs/latest.md (version ${saved.version.id}).`);
