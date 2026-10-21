// Run: node --test templates/portfolio/tests/*.test.mjs
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import * as watcher from "../files/bots/watcher.mjs";
import { renderDigest } from "../files/lib/digest.mjs";
import { fakeCtx } from "../../diligence/tests/fake-ctx.mjs";

const seed = fileURLToPath(new URL("../seed/", import.meta.url));
const RED = ["fernhollow-health", "kestrel-freight-tools", "orchard-analytics", "pocketbook", "quillpoint-payments", "tidemark-security", "wickerlight-learning"];
const AMBER = ["glasswing-pay", "larkfield-labs", "mossgate-robotics", "sable-dynamics"];
const DECOYS = ["dovetail-data", "harrow-health", "marlow-bio", "redwater-pay"];

const files = readdirSync(`${seed}updates`);
const levels = Object.fromEntries(files.map((f) => [f.replace(".md", ""), watcher.assess(watcher.parseUpdates(readFileSync(`${seed}updates/${f}`, "utf8")))]));
const slugsAt = (level) => Object.keys(levels).filter((s) => levels[s].level === level).sort();

test("the seed has 30 companies with six updates each", () => {
  assert.equal(files.length, 30);
  const registry = readFileSync(`${seed}companies.csv`, "utf8").trim().split("\n").slice(1).map((l) => l.split(",")[0]).sort();
  assert.deepEqual(registry, files.map((f) => f.replace(".md", "")).sort());
  for (const f of files) assert.equal(watcher.parseUpdates(readFileSync(`${seed}updates/${f}`, "utf8")).length, 6, f);
});

test("flags exactly the companies in the answer key: 7 red, 4 amber, 19 green", () => {
  assert.deepEqual(slugsAt("red"), RED);
  assert.deepEqual(slugsAt("amber"), AMBER);
  assert.equal(slugsAt("green").length, 19);
  for (const decoy of DECOYS) assert.equal(levels[decoy].level, "green", decoy);
});

test("each warning is for the right reason", () => {
  const rules = (slug) => levels[slug].signals.map((s) => `${s.level}:${s.rule}`).sort();
  assert.deepEqual(rules("kestrel-freight-tools"), ["red:runway"]);
  assert.deepEqual(rules("pocketbook"), ["red:revenue"]);
  assert.deepEqual(rules("tidemark-security"), ["amber:language", "red:revenue"]);
  assert.deepEqual(rules("mossgate-robotics"), ["amber:burn"]);
  assert.deepEqual(rules("sable-dynamics"), ["amber:silence"]);
  assert.deepEqual(rules("wickerlight-learning"), ["amber:language", "amber:runway", "amber:team"]);
  assert.match(levels["kestrel-freight-tools"].signals[0].text, /Runway is 5\.1 months \(cash \$500K, net burn \$98K a month\)/);
  assert.match(levels["larkfield-labs"].signals[0].text, /CTO resigned/);
});

test("numbers are read correctly in all three formats", () => {
  const got = watcher.extractMetrics("**Numbers (May)**\n- MRR: $52.0K (+0.0% MoM)\n- Cash: $988K\n- Net burn: $95K/mo\n- Runway: 10.4 months\n- Headcount: 13");
  assert.deepEqual(got, { mrr: 52000, cash: 988000, burn: 95000, runway: 10.4, headcount: 13 });
  const prose = watcher.extractMetrics("We ended April at $32.7K MRR, down 0.9%. We have $1.2M in the bank and we are burning about $112K a month, so runway is about 7.1 months. We are 21 people.");
  assert.deepEqual(prose, { mrr: 32700, cash: 1200000, burn: 112000, runway: 7.1, headcount: 21 });
  const terse = watcher.extractMetrics("May: MRR $31.7K | cash $602K | burn $90K/mo | runway 7 mo | HC 10");
  assert.deepEqual(terse, { mrr: 31700, cash: 602000, burn: 90000, runway: 7, headcount: 10 });
  assert.deepEqual(watcher.extractMetrics("Busy month, heads down on a large deal."), { mrr: null, cash: null, burn: null, runway: null, headcount: null });
  assert.equal(watcher.extractMetrics("Cash on hand: $2.17M and net burn is $130K/mo").cash, 2170000);
});

test("every seeded update yields cash and burn, except the ones that go quiet", () => {
  const missing = files.flatMap((f) => watcher.parseUpdates(readFileSync(`${seed}updates/${f}`, "utf8")).map((u) => ({ f, u }))).filter(({ u }) => u.cash == null);
  assert.deepEqual(missing.map(({ f, u }) => `${f}:${u.date}`), ["sable-dynamics.md:2026-08-04", "sable-dynamics.md:2026-09-02", "sable-dynamics.md:2026-10-05"]);
});

test("a clean company is green, and a short history does not crash", () => {
  const calm = [1, 2, 3].map((i) => ({ date: `2026-0${i}-01`, subject: "", body: "All good.", mrr: 50000 + i * 1000, cash: 2e6, burn: 100000, runway: null, headcount: 10 }));
  assert.equal(watcher.assess(calm).level, "green");
  assert.equal(watcher.assess(calm.slice(0, 1)).level, "green");
});

test("the answer key names the same companies", () => {
  const key = readFileSync(fileURLToPath(new URL("../files/answer-key.md", import.meta.url)), "utf8");
  const section = (title, next) => key.slice(key.indexOf(title), next ? key.indexOf(next) : undefined);
  const names = (text) => [...text.matchAll(/^\| ([A-Z][^|]+?) \|/gm)].map((m) => m[1].toLowerCase().replaceAll(" ", "-")).filter((n) => n !== "company");
  assert.deepEqual(names(section("## Red", "## Amber")).sort(), RED);
  assert.deepEqual(names(section("## Amber", "## Decoys")).sort(), AMBER);
  assert.deepEqual(names(section("## Decoys")).sort(), DECOYS);
});

test("the bot returns findings with citations, and the digest cites every claim", async () => {
  const bank = {};
  for (const f of files) bank[`portfolio/updates/${f}`] = readFileSync(`${seed}updates/${f}`, "utf8");
  bank["portfolio/companies.csv"] = readFileSync(`${seed}companies.csv`, "utf8");
  bank["portfolio/watch-policy.md"] = readFileSync(`${seed}watch-policy.md`, "utf8");
  const ctx = fakeCtx("fund-bank", bank);
  const result = await watcher.handle({ id: "t-1", input: { prefix: "portfolio/" } }, ctx);

  assert.deepEqual(result.data.counts, { red: 7, amber: 4, green: 19 });
  assert.equal(result.data.asOf, "2026-10-05");
  assert.equal(result.data.companies[0].name, "Quillpoint Payments", "worst runway first");
  assert.ok(result.data.companies.every((c) => c.source.path === `portfolio/updates/${c.slug}.md` && /^[0-9a-f]{12}$/.test(c.source.version)));
  assert.ok(ctx.store.has("portfolio/warnings/t-1.md"));
  assert.ok(JSON.stringify(result).length < 50_000, "fits in a task result");

  const md = renderDigest(result);
  const [body, sourcesTable] = md.split("\n## Sources\n");
  const claims = body.split("\n").filter((l) => /^\s*- /.test(l));
  assert.ok(claims.length > 20);
  for (const line of claims) assert.match(line, /\[S\d+\]/, line);
  const rows = [...sourcesTable.matchAll(/^\| S(\d+) \| fund-bank \| (\S+) \| ([0-9a-f]{12}) \|$/gm)];
  const refs = new Set([...body.matchAll(/\[S(\d+)\]/g)].map((m) => m[1]));
  assert.deepEqual([...refs].sort((a, b) => a - b), rows.map((r) => r[1]));
  assert.match(md, /Next step: Ask for a 13-week cash plan/);
});
