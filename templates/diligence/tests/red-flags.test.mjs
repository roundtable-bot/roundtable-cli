// Run: node --test templates/diligence/tests/*.test.mjs
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { findRedFlags, parseCsv, parseNumber, parsePercent, parseTables } from "../files/bots/red-flags.mjs";

const here = (p) => fileURLToPath(new URL(p, import.meta.url));
const readRoom = (dir) => Object.fromEntries(readdirSync(dir).map((f) => [f, readFileSync(`${dir}/${f}`, "utf8")]));

const planted = readRoom(here("../seed/dataroom-acme"));
const clean = readRoom(here("./fixtures/clean-room"));

const PLANTED_IDS = ["arr-includes-one-time", "burn-vs-hiring-plan", "cohort-churn-hidden", "customer-concentration", "deck-model-mismatch", "renewal-cliff"];

test("finds every planted flag in the Acme data room, and nothing else", () => {
  const { flags, skipped } = findRedFlags(planted);
  assert.deepEqual(skipped, [], "every rule must have run");
  assert.deepEqual(flags.map((f) => f.id).sort(), PLANTED_IDS);
});

test("planted flags carry the figures from the answer key", () => {
  const byId = Object.fromEntries(findRedFlags(planted).flags.map((f) => [f.id, f]));
  assert.match(byId["arr-includes-one-time"].detail, /\$4\.80M.*\$3\.74M.*\$88K.*\$1\.06M/);
  assert.match(byId["cohort-churn-hidden"].detail, /61% of logos.*39% annual logo churn.*7% in the deck.*121%.*2024Q1.*266%.*94%/);
  assert.match(byId["burn-vs-hiring-plan"].detail, /17 people.*\$2\.45M.*\$341K.*38 to 40.*10\.0 months.*Jun 2027/);
  assert.match(byId["deck-model-mismatch"].detail, /Gross margin: the deck says 82%.*73%.*Customers: the deck says 148.*121/);
  assert.match(byId["customer-concentration"].detail, /Halden Freight Group.*\$1\.16M.*31%/);
  assert.match(byId["renewal-cliff"].detail, /40% of recurring ARR.*Halden.*Corvane/);
  assert.doesNotMatch(byId["renewal-cliff"].detail, /Ostrander/);
});

test("every flag names its evidence files, which exist in the room", () => {
  for (const f of findRedFlags(planted).flags) {
    assert.ok(f.evidence.length > 0, f.id);
    for (const e of f.evidence) assert.ok(e.file in planted, `${f.id} cites ${e.file}`);
    assert.ok(f.question.length > 10, f.id);
  }
});

test("the answer key lists exactly the flags the rules find", () => {
  const key = readFileSync(here("../files/answer-key.md"), "utf8");
  const listed = [...key.matchAll(/^\| \d \| `([a-z-]+)`/gm)].map((m) => m[1]).sort();
  assert.deepEqual(listed, PLANTED_IDS);
});

test("a clean data room produces no flags, and every metric matches", () => {
  const { flags, metrics, skipped } = findRedFlags(clean);
  assert.deepEqual(skipped, [], "a control that skips rules proves nothing");
  assert.deepEqual(flags, []);
  assert.ok(metrics.length >= 8, "the control should exercise every comparison");
  assert.ok(metrics.every((m) => m.match), JSON.stringify(metrics.filter((m) => !m.match)));
});

test("a missing file skips its rules instead of passing or throwing", () => {
  const rest = Object.fromEntries(Object.entries(planted).filter(([name]) => name !== "cohorts.csv"));
  const { flags, skipped } = findRedFlags(rest);
  assert.deepEqual(skipped.map((s) => s.rule), ["cohort-churn-hidden"]);
  assert.match(skipped[0].reason, /missing cohorts\.csv/);
  assert.ok(flags.some((f) => f.id === "arr-includes-one-time"));
});

test("a reshaped file is reported as unreadable, not as clean", () => {
  const broken = { ...planted, "financial-model.csv": planted["financial-model.csv"].replace("subscription_revenue", "subs") };
  const { skipped } = findRedFlags(broken);
  assert.ok(skipped.length >= 4);
  assert.match(skipped[0].reason, /could not read the files/);
});

test("each rule fires on its own change to an otherwise clean room", () => {
  const flagIds = (file, from, to) => {
    assert.ok(clean[file].includes(from), `${from} is in ${file}`);
    return findRedFlags({ ...clean, [file]: clean[file].replace(from, to) }).flags.map((f) => f.id);
  };
  assert.deepEqual(flagIds("deck.md", "| ARR | $2.16M |", "| ARR | $2.60M |"), ["arr-includes-one-time"]);
  assert.deepEqual(flagIds("deck.md", "| Annual logo churn | 11% |", "| Annual logo churn | 1% |"), ["cohort-churn-hidden"]);
  assert.deepEqual(flagIds("deck.md", "| Gross margin (Q3 2026) | 78% |", "| Gross margin (Q3 2026) | 90% |"), ["deck-model-mismatch"]);
  assert.deepEqual(flagIds("contracts-summary.md", "| Larkfield Dairy | Growth | 172,000 |", "| Larkfield Dairy | Growth | 700,000 |"), ["customer-concentration"]);
  const noticeIds = findRedFlags({ ...clean, "contracts-summary.md": clean["contracts-summary.md"].replaceAll("| Yes | None |", "| Yes | 30 days' notice |") });
  assert.deepEqual(noticeIds.flags.map((f) => f.id), ["renewal-cliff"]);
  const bigPlan = clean["hiring-plan.csv"] + "VP Engineering,Engineering,2026-10,300000,40000\n";
  assert.deepEqual(findRedFlags({ ...clean, "hiring-plan.csv": bigPlan }).flags.map((f) => f.id), ["burn-vs-hiring-plan"]);
});

test("parsers", () => {
  assert.equal(parseNumber("$4.8M"), 4_800_000);
  assert.equal(parseNumber("$310K"), 310_000);
  assert.equal(parseNumber("1,160,000"), 1_160_000);
  assert.equal(parseNumber("15 months"), 15);
  assert.equal(parseNumber("n/a"), null);
  assert.equal(parsePercent("under 10%"), 0.1);
  assert.equal(parsePercent("none"), null);
  assert.deepEqual(parseCsv('a,b\n1,"x, y"\n'), [{ a: "1", b: "x, y" }]);
  const [t] = parseTables("text\n\n| K | V |\n|---|---|\n| a | 1 |\n| b | 2 |\n\nmore");
  assert.deepEqual(t.rows, [{ K: "a", V: "1" }, { K: "b", V: "2" }]);
});
