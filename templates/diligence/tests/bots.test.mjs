// Run: node --test templates/diligence/tests/*.test.mjs
import assert from "node:assert/strict";
import { test } from "node:test";
import { SAMPLE_COMPS, loadComps, parseCompanyFacts } from "../files/bots/comps.mjs";
import { SYNTHETIC_SIGNALS, loadSignals } from "../files/bots/market.mjs";

// A trimmed SEC "companyfacts" response: two fiscal years, plus a quarterly row to be ignored.
const fact = (start, end, val, accn, extra = {}) => ({ start, end, val, accn, fy: 2024, fp: "FY", form: "10-K", filed: `${end.slice(0, 4)}-09-01`, ...extra });
const facts = (name, cik) => ({
  cik,
  entityName: name,
  facts: {
    "us-gaap": {
      Revenues: { units: { USD: [
        fact("2022-07-01", "2023-06-30", 1000, "0001-23-000001"),
        fact("2023-07-01", "2024-06-30", 1250, "0001-24-000002"),
        fact("2024-04-01", "2024-06-30", 330, "0001-24-000002", { fp: "Q4", form: "10-Q" }),
      ] } },
      GrossProfit: { units: { USD: [fact("2023-07-01", "2024-06-30", 1000, "0001-24-000002")] } },
    },
  },
});

test("parseCompanyFacts reads the latest full-year revenue, growth and margin", () => {
  const comp = parseCompanyFacts(facts("Example Corp", 1234), "EXMP");
  assert.deepEqual(comp, {
    ticker: "EXMP",
    name: "Example Corp",
    fiscalYearEnd: "2024-06-30",
    revenueUsd: 1250,
    growthPct: 25,
    grossMarginPct: 80,
    filing: "https://www.sec.gov/Archives/edgar/data/1234/000124000002/",
  });
  assert.equal(parseCompanyFacts({ entityName: "Empty", facts: {} }, "NONE"), null);
});

test("comps uses SEC EDGAR with the configured User-Agent", async () => {
  const seen = [];
  const fetchImpl = async (url, init) => {
    seen.push({ url, ua: init.headers["User-Agent"] });
    const body = url.endsWith("company_tickers.json")
      ? { 0: { cik_str: 1234, ticker: "EXMP", title: "Example Corp" } }
      : facts("Example Corp", 1234);
    return { ok: true, json: async () => body };
  };
  const result = await loadComps({ userAgent: "Test Team test@example.com", tickers: ["EXMP", "GONE"], fetchImpl });
  assert.equal(result.source, "sec-edgar");
  assert.deepEqual(result.comps.map((c) => c.ticker), ["EXMP"]);
  assert.match(result.note, /GONE/);
  assert.ok(seen.every((r) => r.ua === "Test Team test@example.com"));
  assert.ok(seen.some((r) => r.url === "https://data.sec.gov/api/xbrl/companyfacts/CIK0000001234.json"));
});

test("comps falls back to labelled samples without a User-Agent or when EDGAR fails", async () => {
  const unset = await loadComps({ userAgent: "", fetchImpl: () => assert.fail("must not call the network") });
  assert.equal(unset.source, "sample");
  assert.match(unset.note, /SEC_USER_AGENT is not set/);
  assert.deepEqual(unset.comps, SAMPLE_COMPS);
  assert.ok(SAMPLE_COMPS.every((c) => c.name.includes("(sample)")));

  const offline = await loadComps({ userAgent: "x y@z.test", fetchImpl: async () => { throw new Error("offline"); } });
  assert.equal(offline.source, "sample");
  assert.match(offline.note, /offline/);

  const blocked = await loadComps({ userAgent: "x y@z.test", fetchImpl: async () => ({ ok: false, status: 403 }) });
  assert.equal(blocked.source, "sample");
  assert.match(blocked.note, /403/);
});

test("market returns labelled synthetic signals, with or without Akta settings", async () => {
  const unset = await loadSignals({ apiUrl: "", apiKey: "" });
  assert.equal(unset.source, "synthetic");
  assert.match(unset.note, /AKTA_API_URL or AKTA_API_KEY is not set/);
  assert.deepEqual(unset.signals, SYNTHETIC_SIGNALS);
  assert.ok(SYNTHETIC_SIGNALS.every((s) => s.label === "synthetic"));

  const configured = await loadSignals({ apiUrl: "https://akta.example.test", apiKey: "k" });
  assert.equal(configured.source, "synthetic");
  assert.match(configured.note, /Akta is configured but the Akta adapter is not wired up yet/);
});
