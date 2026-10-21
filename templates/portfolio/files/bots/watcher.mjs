// watcher: reads each portfolio company's monthly founder updates and raises early warnings.
//
// Founders write updates their own way, so the bot first pulls numbers out of free text
// (extractMetrics), then applies the rules in plain code (assess) that mirror
// portfolio/watch-policy.md in the bank. Edit the rules here and the policy there. Everything
// except handle() is a pure function, so you can try it without Roundtable. An LLM can read
// the updates the rules cannot parse at the marked spot in handle().

import { parseCsv } from "../lib/parse.mjs";

export const id = "watcher";
export const skills = ["Extract runway, burn, MRR and headcount from founder updates", "Raise early warnings for a portfolio"];

// ---- reading an update --------------------------------------------------------------------

const AMOUNT = String.raw`\$(\d[\d,]*(?:\.\d+)?)\s*([kKmM])?`;
const SCALE = { k: 1e3, m: 1e6 };
const money = (m) => Math.round(Number(m[1].replaceAll(",", "")) * (SCALE[(m[2] ?? "").toLowerCase()] ?? 1));
const firstMatch = (patterns, text) => patterns.map((p) => p.exec(text)).find(Boolean) ?? null;

/** Numbers from the free text of one update. Anything not found is null. */
export function extractMetrics(text) {
  const mrr = firstMatch([new RegExp(String.raw`MRR[\s:]*(?:(?:is|was|of|at)[\s:]*)?${AMOUNT}`, "i"), new RegExp(String.raw`${AMOUNT}\s*(?:in\s+)?MRR`, "i")], text);
  const cash = firstMatch([new RegExp(String.raw`\bcash(?:\s+on\s+hand|\s+balance)?[\s:]*(?:(?:is|was|of|at)[\s:]*)?${AMOUNT}`, "i"), new RegExp(String.raw`${AMOUNT}\s*(?:in the bank|in cash)`, "i")], text);
  const burn = firstMatch([new RegExp(String.raw`\bburn(?:ing)?[\s:]*(?:(?:is|was|of|about|roughly)[\s:]*)*${AMOUNT}`, "i")], text);
  const runway = firstMatch([/runway[\s:]*(?:(?:is|of|about|roughly|~)[\s:]*)*(\d+(?:\.\d+)?)\s*(?:months|mo)\b/i, /(\d+(?:\.\d+)?)\s*months of runway/i], text);
  const headcount = firstMatch([/headcount[\s:]*(?:is[\s:]*)?(\d+)/i, /\bHC[\s:]*(\d+)/, /team of (\d+)/i, /we are (\d+) people/i], text);
  return {
    mrr: mrr && money(mrr),
    cash: cash && money(cash),
    burn: burn && money(burn),
    runway: runway && Number(runway[1]),
    headcount: headcount && Number(headcount[1]),
  };
}

/** A company file -> its updates, oldest first, each with date, subject, body and metrics. */
export function parseUpdates(markdown) {
  return markdown
    .split(/^---\s*$/m)
    .filter((chunk) => /^Date:/m.test(chunk))
    .map((chunk) => {
      const [headers, ...rest] = chunk.trim().split(/\n\s*\n/);
      const body = rest.join("\n\n");
      return { date: /^Date:\s*(\S+)/m.exec(headers)[1], subject: /^Subject:\s*(.+)$/m.exec(headers)?.[1] ?? "", body, ...extractMetrics(body) };
    })
    .sort((a, b) => a.date.localeCompare(b.date));
}

// ---- the rules ----------------------------------------------------------------------------

const RISK_LANGUAGE = [
  ["resignation", /\b(resigned|resignation|stepping down)\b/i],
  ["layoff", /\b(laid off|layoffs?)\b/i],
  ["pivot", /\bpivot(ing)?\b/i],
  ["delay", /\bdelayed\b|\bbehind schedule\b/i],
  ["lost customer", /\bdid not renew\b|\bdidn'?t renew\b/i],
];

const kmoney = (n) => (n >= 1e6 ? `$${(n / 1e6).toFixed(1)}M` : `$${Math.round(n / 1e3)}K`);
const month = (date) => new Date(`${date}T00:00:00Z`).toLocaleString("en-US", { month: "long", timeZone: "UTC" });
const defined = (updates, key) => updates.filter((u) => u[key] != null);

/**
 * updates: from parseUpdates (oldest first). Returns { level, signals, latest }.
 * Each signal is { rule, level: "red" | "amber", text, date }; `date` is the update it comes from.
 */
export function assess(updates) {
  const signals = [];
  const add = (rule, level, text, update) => signals.push({ rule, level, text, date: update.date });
  const latest = updates.at(-1);

  // Runway: from the latest update that reports cash and burn (or a stated runway).
  const withCash = [...updates].reverse().find((u) => (u.cash != null && u.burn) || u.runway != null);
  const runway = withCash && (withCash.cash != null && withCash.burn ? withCash.cash / withCash.burn : withCash.runway);
  if (runway != null && runway < 9) {
    const basis = withCash.cash != null && withCash.burn ? ` (cash ${kmoney(withCash.cash)}, net burn ${kmoney(withCash.burn)} a month)` : "";
    const stale = withCash === latest ? "" : `, last reported in the ${month(withCash.date)} update`;
    add("runway", runway < 6 ? "red" : "amber", `Runway is ${runway.toFixed(1)} months${basis}${stale}.`, withCash);
  }

  // Revenue: three straight falls, or 20% below the peak, is red; two straight falls is amber.
  const mrr = defined(updates, "mrr");
  if (mrr.length >= 2) {
    const values = mrr.map((u) => u.mrr);
    const last = mrr.at(-1);
    const peak = Math.max(...values);
    const falls = (n) => values.length > n && values.slice(-(n + 1)).every((v, i, a) => i === 0 || v < a[i - 1]);
    if (falls(3)) add("revenue", "red", `MRR fell three months in a row, from ${kmoney(values.at(-4))} to ${kmoney(last.mrr)}.`, last);
    else if (last.mrr <= peak * 0.8) add("revenue", "red", `MRR is ${Math.round((1 - last.mrr / peak) * 100)}% below its peak of ${kmoney(peak)}, at ${kmoney(last.mrr)}.`, last);
    else if (falls(2)) add("revenue", "amber", `MRR fell two months in a row, to ${kmoney(last.mrr)}.`, last);
  }

  // Burn creeping up.
  const burn = defined(updates, "burn");
  if (burn.length >= 5 && burn.at(-1).burn >= burn.at(-5).burn * 1.3) {
    add("burn", "amber", `Net burn rose ${Math.round((burn.at(-1).burn / burn.at(-5).burn - 1) * 100)}% in four months, from ${kmoney(burn.at(-5).burn)} to ${kmoney(burn.at(-1).burn)} a month.`, burn.at(-1));
  }

  // Team shrinking.
  const people = defined(updates, "headcount");
  if (people.length >= 2) {
    const peak = Math.max(...people.map((u) => u.headcount));
    const last = people.at(-1);
    if (last.headcount <= peak * 0.85) add("team", "amber", `Headcount fell from ${peak} to ${last.headcount}.`, last);
  }

  // Risk language in the last three updates, quoting the sentence.
  const quotes = [];
  for (const u of updates.slice(-3)) {
    for (const sentence of u.body.split(/(?<=[.!?])\s+|\n/)) {
      const hit = RISK_LANGUAGE.find(([, re]) => re.test(sentence));
      if (hit) quotes.push({ label: hit[0], sentence: sentence.replace(/^[-*\s]+/, "").replace(/^(Good news|Not so good|Ask):\s*/i, "").trim(), update: u });
    }
  }
  if (quotes.length) {
    const labels = [...new Set(quotes.map((q) => q.label))];
    const latestQuote = quotes.at(-1);
    add("language", "amber", `Updates mention ${labels.join(", ")}: "${latestQuote.sentence}"`, latestQuote.update);
  }

  // Going dark.
  const lastTwo = updates.slice(-2);
  if (lastTwo.length === 2 && lastTwo.every((u) => u.cash == null && u.burn == null && u.runway == null)) {
    add("silence", "amber", "The last two updates report no cash, burn or runway.", latest);
  }

  const reds = signals.filter((s) => s.level === "red").length;
  const ambers = new Set(signals.filter((s) => s.level === "amber").map((s) => s.rule)).size;
  const level = reds || ambers >= 2 ? "red" : ambers ? "amber" : "green";
  return {
    level,
    signals,
    latest: { date: latest.date, mrr: mrr.at(-1)?.mrr ?? null, cash: withCash?.cash ?? null, burn: burn.at(-1)?.burn ?? null, runway: runway ?? null, headcount: people.at(-1)?.headcount ?? null },
  };
}

// ---- LLM hook -----------------------------------------------------------------------------
// Updates the regexes cannot read (a table, a screenshot's text, "we have about nine months")
// come back with null metrics. To read them, send `update.body` to a model here and return
// { mrr, cash, burn, runway, headcount } (numbers or null). Put your API key in .env and never
// commit it.
async function askModel(update) {
  return {};
}

// ---- the bot ------------------------------------------------------------------------------

export async function handle(task, ctx) {
  const prefix = (task.input?.prefix ?? "portfolio/").replace(/\/?$/, "/");
  const entries = await ctx.list(prefix);
  const registryPath = entries.find((e) => e.path === `${prefix}companies.csv`)?.path;
  const registry = registryPath ? await ctx.read(registryPath) : null;
  const names = new Map(registry ? parseCsv(registry.content).map((r) => [r.slug, r]) : []);

  const updatePaths = entries.map((e) => e.path).filter((p) => p.startsWith(`${prefix}updates/`) && p.endsWith(".md"));
  await ctx.progress(`Reading ${updatePaths.length} companies`);
  const companies = [];
  for (const path of updatePaths) {
    const file = await ctx.read(path);
    const slug = path.slice(`${prefix}updates/`.length, -3);
    const updates = parseUpdates(file.content);
    for (const u of updates) if (u.mrr == null && u.cash == null && u.burn == null) Object.assign(u, Object.fromEntries(Object.entries(await askModel(u)).filter(([, v]) => v != null)));
    const company = names.get(slug);
    companies.push({
      slug,
      name: company?.name ?? slug,
      sector: company?.sector ?? null,
      invested: company ? Number(company.invested_usd) : null,
      ownership: company ? Number(company.ownership_pct) : null,
      ...assess(updates),
      source: file.source,
    });
  }

  const order = { red: 0, amber: 1, green: 2 };
  companies.sort((a, b) => order[a.level] - order[b.level] || (a.latest.runway ?? 99) - (b.latest.runway ?? 99) || a.name.localeCompare(b.name));
  const counts = { red: 0, amber: 0, green: 0 };
  for (const c of companies) counts[c.level]++;
  const asOf = companies.map((c) => c.latest.date).sort().at(-1);

  const lines = companies.filter((c) => c.level !== "green").map((c) => `## ${c.name} (${c.level})\n\n${c.signals.map((s) => `- ${s.text}`).join("\n")}\n`);
  const report = `# Portfolio early warnings (${asOf})\n\n${counts.red} red, ${counts.amber} amber, ${counts.green} green of ${companies.length} companies.\n\n${lines.join("\n")}`;
  const saved = await ctx.save(`portfolio/warnings/${task.id}.md`, report, "Portfolio early warnings");
  return {
    summary: `${counts.red} red, ${counts.amber} amber, ${counts.green} green`,
    sources: [saved, ...(registry ? [registry.source] : [])],
    data: { asOf, counts, companies, report: saved, registry: registry?.source ?? null },
  };
}
