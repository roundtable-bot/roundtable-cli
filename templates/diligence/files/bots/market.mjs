// market: market signals for the company's category.
//
// Akta adapter: reads AKTA_API_URL and AKTA_API_KEY. The Akta API is NOT wired up yet, so this
// bot returns the bundled SYNTHETIC signals below, labelled as such, until you finish
// fetchAktaSignals().

export const id = "market";
export const skills = ["Summarise market size, funding and hiring signals for a startup's category"];

// Made-up numbers for "mid-market AP automation and close software". Not real market data.
export const SYNTHETIC_SIGNALS = [
  { topic: "Market size", text: "Mid-market AP automation and close software in the US is about $6.1B a year and growing about 14% a year.", asOf: "2026-09" },
  { topic: "Funding", text: "41 funded rounds in finance-operations software in the last 12 months; the median Series A was $11M.", asOf: "2026-09" },
  { topic: "Hiring", text: "Open roles mentioning 'close automation' are up 23% year on year across the 200 largest finance-software employers.", asOf: "2026-09" },
  { topic: "Buyer pain", text: "The median mid-market close takes 7.5 business days; buyers target 4 or fewer.", asOf: "2026-08" },
  { topic: "Competition", text: "Three funded entrants launched AI-assisted reconciliation in the last six months.", asOf: "2026-09" },
  { topic: "Pricing", text: "Median first-year contract value for mid-market AP automation is $18K, rising to $42K for enterprise plans.", asOf: "2026-08" },
].map((s) => ({ ...s, label: "synthetic" }));

/**
 * TODO(wokelo): confirm the Akta API with Wokelo before relying on this. We do not know its
 * endpoints, authentication scheme or response shape, and we have not guessed them.
 *
 * When you know them: call the API using `apiUrl` and `apiKey`, and return an array of
 * { topic, text, asOf, label: "akta" } objects, the same shape as SYNTHETIC_SIGNALS.
 */
async function fetchAktaSignals({ apiUrl, apiKey, company }) {
  throw new Error("the Akta adapter is not wired up yet (confirm the API with Wokelo)");
}

/** Returns { source: "akta" | "synthetic", note, signals }. */
export async function loadSignals({ apiUrl = process.env.AKTA_API_URL, apiKey = process.env.AKTA_API_KEY, company } = {}) {
  let note = "AKTA_API_URL or AKTA_API_KEY is not set, so these are bundled synthetic signals, not real market data.";
  if (apiUrl && apiKey) {
    try {
      return { source: "akta", note: "", signals: await fetchAktaSignals({ apiUrl, apiKey, company }) };
    } catch (err) {
      note = `Akta is configured but ${err.message}, so these are bundled synthetic signals, not real market data.`;
    }
  }
  return { source: "synthetic", note, signals: SYNTHETIC_SIGNALS };
}

function renderReport({ source, note, signals }) {
  const banner = source === "synthetic" ? "> SYNTHETIC DATA. " : "> Source: Akta. ";
  return `# Market signals\n\n${banner}${note}\n\n${signals.map((s) => `- **${s.topic}** (${s.asOf}): ${s.text}`).join("\n")}\n`;
}

export async function handle(task, ctx) {
  const result = await loadSignals({ company: task.input?.company });
  const saved = await ctx.save(`market/${task.id}.md`, renderReport(result), "Market signals for Acme Ledger");
  return { summary: `${result.signals.length} signals (${result.source})`, sources: [saved], data: { ...result, report: saved } };
}
