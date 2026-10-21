// Run: node --test templates/diligence/tests/*.test.mjs
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import * as comps from "../files/bots/comps.mjs";
import * as market from "../files/bots/market.mjs";
import * as redFlags from "../files/bots/red-flags.mjs";
import { renderMemo } from "../files/lib/memo.mjs";
import { fakeCtx } from "./fake-ctx.mjs";

const seedDir = fileURLToPath(new URL("../seed/dataroom-acme", import.meta.url));
const room = Object.fromEntries(readdirSync(seedDir).map((f) => [`dataroom/acme/${f}`, readFileSync(`${seedDir}/${f}`, "utf8")]));

// Runs the three bots on the seed data room and returns what gather() would.
async function runBots() {
  const ctx = fakeCtx("acme-team", room);
  const task = { id: "t-1", input: { prefix: "dataroom/acme/", company: "Acme Ledger, Inc." } };
  delete process.env.SEC_USER_AGENT;
  const run = async (bot) => ({ status: "done", result: await bot.handle(task, ctx) });
  return { ctx, flags: await run(redFlags), comps: await run(comps), market: await run(market) };
}

test("bots return sources and save their reports to the bank", async () => {
  const { ctx, flags } = await runBots();
  const { sources, data } = flags.result;
  assert.equal(data.flags.length, 6);
  assert.ok(data.flags.every((f) => f.sources.length > 0 && f.sources.every((s) => s.bank === "acme-team" && /^[0-9a-f]{12}$/.test(s.version))));
  assert.ok(sources.some((s) => s.path === "dataroom/acme/deck.md"));
  assert.deepEqual(data.report, { bank: "acme-team", path: "flags/t-1.md", version: sources.at(-1).version });
  assert.ok(ctx.store.has("flags/t-1.md") && ctx.store.has("comps/t-1.md") && ctx.store.has("market/t-1.md"));
});

test("every claim in the memo has a citation that resolves to a bank, path and version", async () => {
  const { flags, comps, market } = await runBots();
  const memo = renderMemo({ company: "Acme Ledger, Inc.", generatedOn: "2026-10-20", flags, comps, market });
  const [body, sourcesSection] = memo.split("\n## Sources\n");

  const lines = body.split("\n");
  const claims = lines.filter((line, i) => /^(- |\d+\. |\| )/.test(line) && !lines[i + 1]?.startsWith("|---") && !line.startsWith("|---"));
  assert.ok(claims.length > 40, `expected many claims, found ${claims.length}`);
  for (const line of claims) assert.match(line, /\[S\d+\]/, `uncited claim: ${line}`);

  const rows = [...sourcesSection.matchAll(/^\| S(\d+) \| (\S+) \| (\S+) \| ([0-9a-f]{12}) \|$/gm)];
  const refs = new Set([...body.matchAll(/\[S(\d+)\]/g)].map((m) => m[1]));
  assert.deepEqual([...refs].sort((a, b) => a - b), rows.map((r) => r[1]), "every cited ref is in the table and vice versa");
  assert.ok(rows.every((r) => r[2] === "acme-team"));
  for (const path of ["dataroom/acme/deck.md", "dataroom/acme/financial-model.csv", "dataroom/acme/cohorts.csv", "flags/t-1.md", "comps/t-1.md", "market/t-1.md"]) {
    assert.ok(rows.some((r) => r[3] === path), `${path} is cited`);
  }
});

test("the memo labels sample and synthetic data, and the red flags are in it", async () => {
  const { flags, comps, market } = await runBots();
  const memo = renderMemo({ company: "Acme Ledger, Inc.", generatedOn: "2026-10-20", flags, comps, market });
  assert.match(memo, /SAMPLE DATA/);
  assert.match(memo, /SYNTHETIC DATA/);
  assert.match(memo, /### 1\. ARR includes one-time implementation fees \(high\)/);
  assert.match(memo, /\| ARR \| \$4\.80M \| \$3\.74M recurring \| \*\*no\*\* \|/);
});

test("a bot that failed leaves an empty section, not an invented one", async () => {
  const { flags } = await runBots();
  const blocked = { status: "blocked", result: { error: "EDGAR exploded" } };
  const memo = renderMemo({ company: "Acme Ledger, Inc.", generatedOn: "2026-10-20", flags, comps: blocked, market: blocked });
  assert.match(memo, /The comps bot did not finish: EDGAR exploded/);
  assert.match(memo, /The market bot did not finish/);
});

test("a claim without a source is an error", () => {
  const noSources = { status: "done", result: { data: { flags: [{ title: "X", severity: "high", detail: "d", question: "q", evidence: [], sources: [] }], metrics: [], skipped: [], report: { bank: "b", path: "p", version: "000000000000" } } } };
  assert.throws(() => renderMemo({ company: "X", generatedOn: "2026-10-20", flags: noSources }), /Claim without a source/);
});
