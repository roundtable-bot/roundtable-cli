// comps: finds public comparable companies and their latest annual revenue, growth and gross
// margin from SEC EDGAR (data.sec.gov). Without SEC_USER_AGENT, or when EDGAR cannot be reached,
// it returns bundled SAMPLE comps, clearly labelled, so the rest of the pipeline still runs.
//
// SEC requires a User-Agent that identifies you, for example:
//   SEC_USER_AGENT="Your Name your.name@example.com"
// Put it in .env. Fair-access policy: https://www.sec.gov/os/accessing-edgar-data

export const id = "comps";
export const skills = ["Find public comparables and their revenue growth and gross margin from SEC filings"];

// Public companies in finance operations software. Change this list to change the comps.
export const TICKERS = ["BILL", "BL", "WK", "EXFY"];

// Fictional companies with made-up numbers. Used only when EDGAR is unavailable.
export const SAMPLE_COMPS = [
  { ticker: "LDGL", name: "Ledgerline Systems (sample)", fiscalYearEnd: "2025-12-31", revenueUsd: 412_000_000, growthPct: 17, grossMarginPct: 74 },
  { ticker: "CLSW", name: "CloseWorks Inc. (sample)", fiscalYearEnd: "2025-12-31", revenueUsd: 268_000_000, growthPct: 21, grossMarginPct: 79 },
  { ticker: "PYBR", name: "PayBridge Holdings (sample)", fiscalYearEnd: "2025-06-30", revenueUsd: 1_120_000_000, growthPct: 12, grossMarginPct: 68 },
  { ticker: "TLHS", name: "Tallyhouse Software (sample)", fiscalYearEnd: "2025-12-31", revenueUsd: 154_000_000, growthPct: 26, grossMarginPct: 81 },
].map((c) => ({ ...c, filing: null }));

const REVENUE_TAGS = ["RevenueFromContractWithCustomerExcludingAssessedTax", "Revenues", "SalesRevenueNet"];
const DAY = 864e5;

/** Annual (10-K, full-year) values of one us-gaap tag, newest first. */
function annualFacts(facts, tag) {
  const byEnd = new Map();
  for (const f of facts?.["us-gaap"]?.[tag]?.units?.USD ?? []) {
    const days = (Date.parse(f.end) - Date.parse(f.start)) / DAY;
    if (f.form !== "10-K" || f.fp !== "FY" || !(days >= 350 && days <= 380)) continue;
    if (!byEnd.has(f.end) || f.filed > byEnd.get(f.end).filed) byEnd.set(f.end, f);
  }
  return [...byEnd.values()].sort((a, b) => b.end.localeCompare(a.end));
}

/** SEC "companyfacts" JSON -> one comp row, or null if the company has no usable revenue. */
export function parseCompanyFacts(json, ticker) {
  const series = REVENUE_TAGS.map((tag) => annualFacts(json.facts, tag))
    .filter((s) => s.length)
    .sort((a, b) => b[0].end.localeCompare(a[0].end))[0];
  if (!series) return null;
  const [latest, prior] = series;
  const gross = annualFacts(json.facts, "GrossProfit").find((f) => f.end === latest.end);
  const yearApart = prior && Math.abs((Date.parse(latest.end) - Date.parse(prior.end)) / DAY - 365) < 15;
  return {
    ticker,
    name: json.entityName,
    fiscalYearEnd: latest.end,
    revenueUsd: latest.val,
    growthPct: yearApart ? Math.round(((latest.val / prior.val) - 1) * 100) : null,
    grossMarginPct: gross ? Math.round((gross.val / latest.val) * 100) : null,
    filing: `https://www.sec.gov/Archives/edgar/data/${Number(json.cik)}/${latest.accn.replaceAll("-", "")}/`,
  };
}

/**
 * Returns { source: "sec-edgar" | "sample", note, comps }.
 * `fetchImpl` can be replaced in tests.
 */
export async function loadComps({ userAgent = process.env.SEC_USER_AGENT, tickers = TICKERS, fetchImpl = fetch } = {}) {
  const sample = (note) => ({ source: "sample", note, comps: SAMPLE_COMPS });
  if (!userAgent) return sample("SEC_USER_AGENT is not set, so these are bundled sample comps, not real filings.");

  const get = async (url) => {
    const res = await fetchImpl(url, { headers: { "User-Agent": userAgent, Accept: "application/json" }, signal: AbortSignal.timeout(15_000) });
    if (!res.ok) throw new Error(`${url} returned HTTP ${res.status}`);
    return res.json();
  };

  try {
    // SEC's ticker -> CIK list lives on www.sec.gov; the financial facts come from data.sec.gov.
    const directory = Object.values(await get("https://www.sec.gov/files/company_tickers.json"));
    const comps = [];
    const missing = [];
    for (const ticker of tickers) {
      const entry = directory.find((e) => e.ticker === ticker);
      const facts = entry && (await get(`https://data.sec.gov/api/xbrl/companyfacts/CIK${String(entry.cik_str).padStart(10, "0")}.json`));
      const comp = facts && parseCompanyFacts(facts, ticker);
      if (comp) comps.push(comp);
      else missing.push(ticker);
    }
    if (!comps.length) return sample(`EDGAR returned no usable revenue for ${tickers.join(", ")}, so these are bundled sample comps.`);
    return { source: "sec-edgar", note: missing.length ? `No usable annual revenue found for: ${missing.join(", ")}.` : "", comps };
  } catch (err) {
    return sample(`Could not reach SEC EDGAR (${err.message}), so these are bundled sample comps, not real filings.`);
  }
}

const money = (n) => (n >= 1e9 ? `$${(n / 1e9).toFixed(2)}B` : `$${Math.round(n / 1e6)}M`);

function renderReport({ source, note, comps }) {
  const banner = source === "sample" ? "> SAMPLE DATA: fictional companies with made-up numbers. " : "> Source: SEC EDGAR 10-K filings. ";
  const rows = comps.map((c) => `| ${c.name} | ${c.ticker} | ${c.fiscalYearEnd} | ${money(c.revenueUsd)} | ${c.growthPct ?? "n/a"}% | ${c.grossMarginPct ?? "n/a"}% | ${c.filing ?? "n/a"} |`);
  return `# Comparable companies\n\n${banner}${note}\n\n| Company | Ticker | Fiscal year end | Revenue | Growth | Gross margin | Filing |\n|---|---|---|---|---|---|---|\n${rows.join("\n")}\n`;
}

export async function handle(task, ctx) {
  await ctx.progress(process.env.SEC_USER_AGENT ? "Querying SEC EDGAR" : "SEC_USER_AGENT not set; using sample comps");
  const result = await loadComps();
  const saved = await ctx.save(`comps/${task.id}.md`, renderReport(result), "Comparable companies for Acme Ledger");
  return { summary: `${result.comps.length} comps from ${result.source}`, sources: [saved], data: { ...result, report: saved } };
}
