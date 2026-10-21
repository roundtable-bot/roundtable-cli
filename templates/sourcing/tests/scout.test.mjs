// Run: node --test templates/sourcing/tests/*.test.mjs
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import * as scout from "../files/bots/scout.mjs";
import { renderShortlist } from "../files/lib/shortlist.mjs";
import { fakeCtx } from "../../diligence/tests/fake-ctx.mjs";

const seed = fileURLToPath(new URL("../seed/", import.meta.url));
const TODAY = "2026-10-15";
const STANDOUTS = ["Anjali Rostami-Boyce", "Ifeoma Achterberg", "Maren Dalgaard", "Soren Vukovic-Lang", "Tomasz Hedgecock", "Yuki Bellamy-Okoro"];
const DECOYS = ["Beatrix Nwosu-Tamm", "Casimir Odenkirk", "Dmitri Alvarenga", "Gideon Prowse", "Leocadia Marchetti", "Priyanka Thorsen-Whelan"];

const read = (dir) => readdirSync(`${seed}${dir}`).map((f) => readFileSync(`${seed}${dir}/${f}`, "utf8"));
const profiles = read("profiles").map(scout.parseProfile);
const filings = read("filings").map(scout.parseFiling);
const ranked = scout.rankFounders(profiles, filings, TODAY);
const names = (rows) => rows.map((r) => r.profile.name).sort();

test("the seed has 40 founders and Form D-style filings", () => {
  assert.equal(profiles.length, 40);
  assert.ok(filings.length >= 12);
  assert.ok(profiles.every((p) => p.name && p.company && p.signals.length > 0));
});

test("the six standouts are the top six, with a clear gap to the rest", () => {
  assert.deepEqual(names(ranked.slice(0, 6)), STANDOUTS);
  assert.ok(ranked[5].score - ranked[6].score >= 3, `gap ${ranked[5].score} vs ${ranked[6].score}`);
});

test("no decoy makes the top ten", () => {
  const topTen = names(ranked.slice(0, 10));
  for (const decoy of DECOYS) assert.ok(!topTen.includes(decoy), `${decoy} ranked too high`);
});

test("timing comes from the Form D: open round is a plus, a closed Series A a minus", () => {
  const by = Object.fromEntries(ranked.map((r) => [r.profile.name, r]));
  assert.equal(by["Soren Vukovic-Lang"].timing.points, 1);
  assert.match(by["Soren Vukovic-Lang"].timing.text, /round is open now/);
  assert.equal(by["Ifeoma Achterberg"].timing.points, 1);
  assert.equal(by["Priyanka Thorsen-Whelan"].timing.points, -3);
  assert.match(by["Maren Dalgaard"].timing.text, /No Form D on file/);
  assert.equal(by["Tomasz Hedgecock"].timing.points, 0);
});

test("lists, forks and old prizes earn nothing", () => {
  const score = (name) => ranked.find((r) => r.profile.name === name).score;
  assert.ok(score("Gideon Prowse") <= 1);
  assert.ok(score("Leocadia Marchetti") <= 1);
  assert.ok(score("Beatrix Nwosu-Tamm") <= 1);
});

test("the answer key names the same six standouts", () => {
  const key = readFileSync(fileURLToPath(new URL("../files/answer-key.md", import.meta.url)), "utf8");
  const listed = [...key.matchAll(/^\| ([A-Z][^|]+?) \| [^|]+, Inc\. \|/gm)].map((m) => m[1]).sort();
  assert.deepEqual(listed, STANDOUTS);
  for (const decoy of DECOYS) assert.ok(key.includes(decoy), decoy);
});

test("whyMeet is built only from the evidence", () => {
  const why = scout.whyMeet(ranked.find((r) => r.profile.name === "Maren Dalgaard"));
  assert.match(why, /Tracelet, Inc\. \(open-source tracing/);
  assert.match(why, /3,100 weekly active users/);
  assert.match(why, /5 different kinds of public signal/);
});

test("the bot returns picks with citations, and the shortlist cites every claim", async () => {
  const files = {};
  for (const [dir, rows] of [["profiles", read("profiles")], ["filings", read("filings")]]) readdirSync(`${seed}${dir}`).forEach((f, i) => (files[`sourcing/${dir}/${f}`] = rows[i]));
  files["sourcing/thesis.md"] = readFileSync(`${seed}thesis.md`, "utf8");
  const ctx = fakeCtx("fund-bank", files);
  const result = await scout.handle({ id: "t-1", input: { prefix: "sourcing/", top: 8 } }, ctx);

  assert.equal(result.data.today, TODAY, "date comes from thesis.md, not the clock");
  assert.equal(result.data.picks.length, 8);
  assert.deepEqual(result.data.picks.slice(0, 6).map((p) => p.name).sort(), STANDOUTS);
  assert.ok(result.data.picks.every((p) => p.sources[0].path.startsWith("sourcing/profiles/") && /^[0-9a-f]{12}$/.test(p.sources[0].version)));
  assert.deepEqual(result.data.picks.find((p) => p.name === "Soren Vukovic-Lang").sources.map((s) => s.path), ["sourcing/profiles/soren-vukovic-lang.md", "sourcing/filings/form-d-gridwell.md"]);
  assert.ok(ctx.store.has("sourcing/shortlists/t-1.md"));

  const md = renderShortlist(result);
  const [body, sourcesTable] = md.split("\n## Sources\n");
  const claims = body.split("\n").filter((l) => /^(- |\*\*Why|Scanned)/.test(l));
  assert.ok(claims.length > 30);
  for (const line of claims) assert.match(line, /\[S\d+\]/, line);
  const rows = [...sourcesTable.matchAll(/^\| S(\d+) \| fund-bank \| (\S+) \| ([0-9a-f]{12}) \|$/gm)];
  const refs = new Set([...body.matchAll(/\[S(\d+)\]/g)].map((m) => m[1]));
  assert.deepEqual([...refs].sort((a, b) => a - b), rows.map((r) => r[1]));
});
