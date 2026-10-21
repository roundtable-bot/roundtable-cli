// Run: node --test templates/funds/tests/*.test.mjs
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import * as reader from "../files/bots/fund-reader.mjs";
import { fundSlugs, renderBrief } from "../files/lib/brief.mjs";
import { parseAmount } from "../files/lib/parse.mjs";
import { fakeCtx } from "../../diligence/tests/fake-ctx.mjs";

const seed = fileURLToPath(new URL("../seed/", import.meta.url));
const FUNDS = ["cinder-peak-ii", "halyard-iii"];
const bank = { "funds/lp.md": readFileSync(`${seed}lp.md`, "utf8") };
for (const fund of FUNDS) for (const f of readdirSync(`${seed}${fund}`)) bank[`funds/${fund}/${f}`] = readFileSync(`${seed}${fund}/${f}`, "utf8");

async function run(fund) {
  const ctx = fakeCtx("fund-bank", bank);
  return { ctx, result: await reader.handle({ id: `t-${fund}`, input: { fund } }, ctx) };
}

test("amounts: parentheses are negative, suffixes and multiples are understood", () => {
  assert.equal(parseAmount("(1,260)"), -1260);
  assert.equal(parseAmount("$7,200,000"), 7_200_000);
  assert.equal(parseAmount("1.24x"), 1.24);
  assert.equal(parseAmount("0"), 0);
  assert.equal(parseAmount(""), null);
  assert.equal(parseAmount("n/a"), null);
});

test("lp.md lists the fund folders", () => {
  assert.deepEqual(fundSlugs(bank["funds/lp.md"]).sort(), FUNDS);
});

test("reports are read into dollars", () => {
  const q2 = reader.parseReport(bank["funds/halyard-iii/q2-2026-report.md"]);
  assert.equal(q2.quarter, "Q2 2026");
  assert.equal(q2.fundSize, 180_000_000);
  assert.deepEqual(q2.fund, { begin: 107_820_000, contributions: 12_600_000, distributions: 0, fees: -900_000, expenses: -195_000, realized: 0, unrealized: 3_480_000, end: 122_805_000 });
  assert.equal(q2.lp.end, 4_093_500);
  assert.deepEqual(q2.position, { commitment: 6_000_000, paidIn: 3_300_000, cumDist: 0, unfunded: 2_700_000, dpi: 0, rvpi: 1.24, tvpi: 1.24 });
  assert.equal(q2.movers[0].company, "Kestrelon Labs");
  assert.equal(q2.movers[0].change, -9_800_000);
  assert.equal(q2.othersChange, 480_000);

  const call = reader.parseNotice(bank["funds/halyard-iii/capital-call-2026-06-03.md"]);
  assert.deepEqual([call.date, call.total, call.share], ["2026-06-03", 4_900_000, 163_333]);
  const k1 = reader.parseK1(bank["funds/cinder-peak-ii/k1-summary-2025.md"]);
  assert.deepEqual(k1.capital, { begin: 2_450_000, contributed: 240_000, income: 290_000, distributions: -560_000, end: 2_420_000 });
});

test("Halyard: 20 checks, only the Q2 capital calls disagree with their notices", async () => {
  const { result } = await run("halyard-iii");
  const failed = result.data.checks.filter((c) => !c.ok);
  assert.equal(result.data.checks.length, 20);
  assert.deepEqual(failed.map((c) => `${c.quarter}:${c.id}`), ["Q2 2026:calls-vs-notices"]);
  assert.match(failed[0].detail, /\$12,600,000 called \(\$420,000 from you\).*2 notices.*\$12,100,000 \(\$403,333 from you\).*\$16,667/);
});

test("Cinder Peak: 20 checks, the Q2 NAV does not roll forward and your account does not tie to it", async () => {
  const { result } = await run("cinder-peak-ii");
  const failed = result.data.checks.filter((c) => !c.ok);
  assert.equal(result.data.checks.length, 20);
  assert.deepEqual(failed.map((c) => `${c.quarter}:${c.id}`), ["Q2 2026:fund-rollforward", "Q2 2026:lp-vs-fund"]);
  assert.match(failed[0].detail, /\$287,905,000.*\$289,745,000.*-\$1,840,000 \(0\.6% of NAV\)/);
  assert.match(failed[1].detail, /\$2,759,477.*\$2,741,952.*\$17,525/);
});

test("what moved: bridge, movers and multiples", async () => {
  const { result } = await run("halyard-iii");
  const m = result.data.moved;
  assert.deepEqual([m.from, m.to], ["Q1 2026", "Q2 2026"]);
  assert.deepEqual([m.lp.start, m.lp.end, m.lp.change], [3_594_000, 4_093_500, 499_500]);
  assert.equal(m.lp.bridge.reduce((s, b) => s + b.amount, m.lp.start), m.lp.end, "the bridge adds up");
  assert.equal(m.fund.bridge.reduce((s, b) => s + b.amount, m.fund.start), m.fund.end);
  assert.deepEqual(m.movers.map((v) => v.company), ["Kestrelon Labs", "Tessellate AI", "Cobblestone Energy", "Halcyon Security"]);
  assert.deepEqual(m.lp.tvpi, [1.25, 1.24]);
});

test("a fund with one report has no comparison, and a bad fund name is refused", async () => {
  const one = { ...bank };
  delete one["funds/halyard-iii/q1-2026-report.md"];
  const result = await reader.handle({ id: "t", input: { fund: "halyard-iii" } }, fakeCtx("fund-bank", one));
  assert.equal(result.data.moved, null);
  await assert.rejects(reader.handle({ id: "t", input: { fund: "../etc" } }, fakeCtx("fund-bank", bank)), /fund folder name/);
  await assert.rejects(reader.handle({ id: "t", input: { fund: "no-such-fund" } }, fakeCtx("fund-bank", bank)), /No quarterly reports/);
});

test("the answer key names the failing checks", () => {
  const key = readFileSync(fileURLToPath(new URL("../files/answer-key.md", import.meta.url)), "utf8");
  for (const id of ["calls-vs-notices", "fund-rollforward", "lp-vs-fund"]) assert.ok(key.includes(`\`${id}\``), id);
  assert.match(key, /\$12,600,000/);
  assert.match(key, /\$287,905K/);
});

test("the brief cites every claim, and its sources resolve", async () => {
  const results = [];
  for (const fund of fundSlugs(bank["funds/lp.md"])) results.push((await run(fund)).result);
  const md = renderBrief(results);
  const [body, sourcesTable] = md.split("\n## Sources\n");
  const lines = body.split("\n");
  const claims = lines.filter((l, i) => /^(- |\| )/.test(l) && !lines[i + 1]?.startsWith("|---") && !l.startsWith("|---"));
  assert.ok(claims.length > 30);
  for (const line of claims) assert.match(line, /\[S\d+\]/, line);
  const rows = [...sourcesTable.matchAll(/^\| S(\d+) \| fund-bank \| (\S+) \| ([0-9a-f]{12}) \|$/gm)];
  const refs = new Set([...body.matchAll(/\[S(\d+)\]/g)].map((m) => m[1]));
  assert.deepEqual([...refs].sort((a, b) => a - b), rows.map((r) => r[1]));
  assert.match(md, /3 of 40 cross-checks between the documents failed/);
  assert.deepEqual([...md.matchAll(/(\d+) other checks passed/g)].map((m) => m[1]), ["19", "18"]);
  assert.match(md, /Kestrelon Labs: -\$9\.8M/);
});
