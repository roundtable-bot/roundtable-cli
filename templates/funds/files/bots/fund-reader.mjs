// fund-reader: reads one fund's quarterly reports, capital call and distribution notices and K-1
// summary, pulls out NAV, capital calls, distributions and valuation changes, explains what moved
// since last quarter, and checks that the documents agree with each other.
//
// Everything except handle() is a pure function, so you can try it without Roundtable. The
// reports are semi-structured: rows are found by their label (see the ROWS tables below), so a
// report that words a row differently is skipped, not misread. An LLM can read the free-text
// parts (the manager commentary) at the marked spot in handle().

import { field, parseAmount, parseTables, sections } from "../lib/parse.mjs";

export const id = "fund-reader";
export const skills = ["Extract NAV, capital calls, distributions and valuation changes from fund reports and K-1s", "Explain what moved in a fund since last quarter"];

// ---- reading the documents ----------------------------------------------------------------

// Row label -> key. The first rule whose pattern matches the label wins.
const ROWS = {
  fund: [["begin", /^NAV, beginning/i], ["contributions", /^Capital contributions/i], ["distributions", /^Distributions/i], ["fees", /^Management fees/i], ["expenses", /^Fund expenses/i], ["realized", /^Net realized/i], ["unrealized", /^Net change in unrealized/i], ["end", /^NAV, end/i]],
  lp: [["begin", /^Beginning capital/i], ["contributions", /^Capital contributions/i], ["distributions", /^Distributions/i], ["fees", /^Management fees/i], ["realized", /^Net realized/i], ["unrealized", /^Net change in unrealized/i], ["end", /^Ending capital/i]],
  position: [["commitment", /^Commitment/i], ["paidIn", /^Paid-in/i], ["cumDist", /^Cumulative distributions/i], ["unfunded", /^Unfunded/i], ["dpi", /^DPI/i], ["rvpi", /^RVPI/i], ["tvpi", /^TVPI/i]],
};

function readRows(sectionText, rules, scale) {
  const table = parseTables(sectionText)[0];
  if (!table) return null;
  const out = {};
  for (const row of table.rows) {
    const key = rules.find(([, re]) => re.test(row[table.header[0]]))?.[0];
    const value = parseAmount(row[table.header[1]]);
    if (key && value != null) out[key] = ["dpi", "rvpi", "tvpi"].includes(key) ? value : value * scale;
  }
  return out;
}

const section = (sects, re) => Object.entries(sects).find(([title]) => re.test(title))?.[1] ?? "";

/** One quarterly report -> { quarter, fundSize, fund, lp, position, movers, othersChange }. All amounts in dollars. */
export function parseReport(markdown) {
  const sects = sections(markdown);
  const valuation = parseTables(section(sects, /valuation changes/i))[0];
  const movers = (valuation?.rows ?? []).filter((r) => !/^All other/i.test(r.Company)).map((r) => ({
    company: r.Company,
    prior: parseAmount(r["Prior fair value"]) * 1000,
    current: parseAmount(r["Current fair value"]) * 1000,
    change: parseAmount(r.Change) * 1000,
    basis: r.Basis,
  }));
  const others = valuation?.rows.find((r) => /^All other/i.test(r.Company));
  return {
    fundName: /^# (.+?):/m.exec(markdown)?.[1],
    quarter: /quarterly report, (Q\d \d{4})/.exec(markdown)?.[1],
    issued: field(markdown, "Issued"),
    fundSize: parseAmount(field(markdown, "Fund size \\(total commitments\\)")),
    fund: readRows(section(sects, /^Fund summary/i), ROWS.fund, 1000),
    lp: readRows(section(sects, /^Your capital account/i), ROWS.lp, 1),
    position: readRows(section(sects, /^Your commitment/i), ROWS.position, 1),
    movers,
    othersChange: others ? parseAmount(others.Change) * 1000 : 0,
  };
}

/** "$7,200,000 (4.0000% of total commitments)" -> 7200000. */
const amountIn = (text) => parseAmount(String(text).replace(/\s*\(.*$/, ""));

/** A capital call or distribution notice -> { date, total, share }. */
export function parseNotice(markdown) {
  return {
    no: Number(/notice No\. (\d+)/i.exec(markdown)?.[1]),
    date: field(markdown, "Date"),
    total: amountIn(field(markdown, "Total call") ?? field(markdown, "Total distribution")),
    share: amountIn(field(markdown, "Your share of this call") ?? field(markdown, "Your share of this distribution")),
    purpose: parseTables(markdown)[0]?.rows.map((r) => r.Purpose ?? r.Component) ?? [],
  };
}

/** K-1 summary -> { year, cumContributions, cumDistributions, capital: { begin, contributed, income, distributions, end } }. */
export function parseK1(markdown) {
  const table = parseTables(section(sections(markdown), /^Item L/i))[0];
  // Read by header name: a header like "2025" is an integer-like key, which JavaScript orders first.
  const item = Object.fromEntries((table?.rows ?? []).map((r) => [r[table.header[0]], parseAmount(r[table.header[1]])]));
  return {
    year: Number(/tax year (\d{4})/.exec(markdown)?.[1]),
    cumContributions: parseAmount(field(markdown, "Cumulative contributions through 31 December \\d{4}")),
    cumDistributions: parseAmount(field(markdown, "Cumulative distributions through 31 December \\d{4}")),
    capital: { begin: item["Beginning capital account"], contributed: item["Capital contributed during the year"], income: item["Current year net income (loss)"], distributions: item["Withdrawals and distributions"], end: item["Ending capital account"] },
  };
}

// ---- checks -------------------------------------------------------------------------------

const sum = (xs) => xs.reduce((a, b) => a + b, 0);
const money = (n) => `${n < 0 ? "-" : ""}$${Math.abs(Math.round(n)).toLocaleString("en-US")}`;
export const quarterOf = (date) => `Q${Math.ceil(Number(date.slice(5, 7)) / 3)} ${date.slice(0, 4)}`;

/**
 * reports: parseReport results, oldest first. calls/distributions: parseNotice results.
 * k1: parseK1 result or null. Returns [{ id, quarter, ok, label, detail }].
 * Tolerances allow for the reports' rounding (fund tables are in thousands).
 */
export function checkFund({ reports, calls = [], distributions = [], k1 = null }) {
  const checks = [];
  const add = (id, quarter, label, ok, detail) => checks.push({ id, quarter, label, ok, detail });

  reports.forEach((r, i) => {
    const q = r.quarter;
    const share = r.position.commitment / r.fundSize;
    const flows = (t) => sum(["contributions", "distributions", "fees", "expenses", "realized", "unrealized"].map((k) => t[k] ?? 0));

    const gap = r.fund.end - (r.fund.begin + flows(r.fund));
    add("fund-rollforward", q, "Fund NAV roll-forward", Math.abs(gap) <= 500,
      Math.abs(gap) <= 500 ? "Opening NAV plus the quarter's flows equals closing NAV." : `The reported closing NAV is ${money(r.fund.end)}, but opening NAV plus the quarter's flows is ${money(r.fund.begin + flows(r.fund))}: ${money(gap)} (${((Math.abs(gap) / r.fund.end) * 100).toFixed(1)}% of NAV) is unexplained.`);

    const lpGap = r.lp.end - (r.lp.begin + flows(r.lp));
    add("lp-rollforward", q, "Your capital account roll-forward", Math.abs(lpGap) <= 2, Math.abs(lpGap) <= 2 ? "Your capital account adds up." : `Your ending capital account is off by ${money(lpGap)}.`);

    const implied = r.fund.end * share;
    const tie = Math.abs(r.lp.end - implied) <= Math.max(5, implied * 0.001);
    add("lp-vs-fund", q, "Your capital account against fund NAV", tie, tie ? "Your ending capital account equals the fund NAV times your share." : `Your ending capital account is ${money(r.lp.end)}; the fund NAV times your ${(share * 100).toFixed(2)}% share is ${money(implied)} (${money(r.lp.end - implied)} apart).`);

    const listed = sum(r.movers.map((m) => m.change)) + r.othersChange;
    add("valuation-sum", q, "Valuation changes add up", Math.abs(listed - r.fund.unrealized) <= 500, Math.abs(listed - r.fund.unrealized) <= 500 ? "The listed valuation changes sum to the reported change in unrealized appreciation." : `The valuation table sums to ${money(listed)} but the summary reports ${money(r.fund.unrealized)}.`);

    const unfunded = r.position.commitment - r.position.paidIn;
    add("unfunded", q, "Unfunded commitment", Math.abs(unfunded - r.position.unfunded) <= 1, Math.abs(unfunded - r.position.unfunded) <= 1 ? "Unfunded commitment equals commitment less paid-in capital." : `Unfunded is reported as ${money(r.position.unfunded)}; commitment less paid-in is ${money(unfunded)}.`);

    const dpi = r.position.cumDist / r.position.paidIn;
    const tvpi = (r.lp.end + r.position.cumDist) / r.position.paidIn;
    const ratiosOk = Math.abs(dpi - r.position.dpi) < 0.006 && Math.abs(tvpi - r.position.tvpi) < 0.006;
    add("multiples", q, "DPI and TVPI", ratiosOk, ratiosOk ? "DPI and TVPI recompute from paid-in, distributions and NAV." : `Reported DPI/TVPI are ${r.position.dpi}x/${r.position.tvpi}x; recomputed ${dpi.toFixed(2)}x/${tvpi.toFixed(2)}x.`);

    const inQuarter = (list) => list.filter((n) => quarterOf(n.date) === q);
    const c = inQuarter(calls);
    const callTotal = sum(c.map((n) => n.total));
    const callShare = sum(c.map((n) => n.share));
    const callsOk = Math.abs(callTotal - r.fund.contributions) <= 1000 && Math.abs(callShare - r.lp.contributions) <= 2;
    add("calls-vs-notices", q, "Capital calls against notices", callsOk, callsOk ? "The report's capital called matches the notices." : `The report shows ${money(r.fund.contributions)} called (${money(r.lp.contributions)} from you), but the ${c.length} notice${c.length === 1 ? "" : "s"} dated in ${q} total ${money(callTotal)} (${money(callShare)} from you): ${money(r.lp.contributions - callShare)} of your capital is unexplained.`);

    const d = inQuarter(distributions);
    const distTotal = sum(d.map((n) => n.total));
    const distShare = sum(d.map((n) => n.share));
    const distOk = Math.abs(distTotal + r.fund.distributions) <= 1000 && Math.abs(distShare + r.lp.distributions) <= 2;
    add("distributions-vs-notices", q, "Distributions against notices", distOk, distOk ? "The report's distributions match the notices." : `The report shows ${money(-r.fund.distributions)} distributed, the notices total ${money(distTotal)}.`);

    const prev = reports[i - 1];
    if (prev) {
      const ok = Math.abs(r.fund.begin - prev.fund.end) <= 500 && Math.abs(r.lp.begin - prev.lp.end) <= 2;
      add("continuity", q, "Opening balances match last quarter's closing", ok, ok ? `${q} opens where ${prev.quarter} closed.` : `${q} opens at ${money(r.fund.begin)} but ${prev.quarter} closed at ${money(prev.fund.end)}.`);
      const paid = Math.abs(r.position.paidIn - (prev.position.paidIn + r.lp.contributions)) <= 2;
      add("paid-in-roll", q, "Paid-in capital rolls forward", paid, paid ? "Paid-in capital equals last quarter's plus this quarter's contributions." : `Paid-in is ${money(r.position.paidIn)}, expected ${money(prev.position.paidIn + r.lp.contributions)}.`);
    } else if (k1) {
      const paid = Math.abs(r.position.paidIn - (k1.cumContributions + r.lp.contributions)) <= 2;
      const dist = Math.abs(r.position.cumDist - (k1.cumDistributions - r.lp.distributions)) <= 2;
      add("k1-tie", q, "Paid-in and distributions against the K-1", paid && dist, paid && dist ? `Paid-in and cumulative distributions equal the ${k1.year} K-1 totals plus ${q}.` : `The K-1 totals plus ${q} give paid-in ${money(k1.cumContributions + r.lp.contributions)} and distributions ${money(k1.cumDistributions - r.lp.distributions)}; the report shows ${money(r.position.paidIn)} and ${money(r.position.cumDist)}.`);
    }
  });

  if (k1) {
    const c = k1.capital;
    const gap = c.end - (c.begin + c.contributed + c.income + c.distributions);
    add("k1-capital", `${k1.year} K-1`, "K-1 capital account analysis", Math.abs(gap) <= 2, Math.abs(gap) <= 2 ? "The K-1's beginning capital plus its flows equals ending capital." : `The K-1's ending capital is ${money(gap)} away from the sum of its parts.`);
  }
  return checks;
}

// ---- what moved ---------------------------------------------------------------------------

/** Compares two consecutive reports: how NAV moved, and the biggest valuation changes. */
export function whatMoved(prev, cur) {
  const bridge = (a, b, fees) => [
    { label: "Capital called", amount: b.contributions },
    { label: "Distributions", amount: b.distributions },
    { label: "Fees and expenses", amount: fees },
    { label: "Realized gains", amount: b.realized },
    { label: "Change in unrealized value", amount: b.unrealized },
  ];
  const movers = [...cur.movers].sort((a, b) => Math.abs(b.change) - Math.abs(a.change)).slice(0, 4);
  return {
    from: prev.quarter,
    to: cur.quarter,
    lp: { start: cur.lp.begin, end: cur.lp.end, change: cur.lp.end - cur.lp.begin, bridge: bridge(cur.lp.begin, cur.lp, cur.lp.fees), dpi: [prev.position.dpi, cur.position.dpi], tvpi: [prev.position.tvpi, cur.position.tvpi] },
    fund: { start: cur.fund.begin, end: cur.fund.end, change: cur.fund.end - cur.fund.begin, bridge: bridge(cur.fund.begin, cur.fund, cur.fund.fees + cur.fund.expenses) },
    movers,
  };
}

// ---- LLM hook -----------------------------------------------------------------------------
// The manager commentary in each report is free text this bot does not read. To summarise it,
// send it to a model here and return a sentence to add to the explanation. Put your API key in
// .env and never commit it.
async function askModel(report) {
  return null;
}

// ---- the bot ------------------------------------------------------------------------------

export async function handle(task, ctx) {
  const fund = task.input?.fund;
  if (!/^[a-z0-9-]+$/.test(fund ?? "")) throw new Error('task.input.fund must be a fund folder name such as "halyard-iii"');
  const prefix = `${task.input?.prefix ?? "funds/"}`.replace(/\/?$/, "/") + `${fund}/`;
  const entries = await ctx.list(prefix);
  await ctx.progress(`Reading ${entries.length} documents for ${fund}`);

  const files = {};
  const reports = [];
  const calls = [];
  const distributions = [];
  let k1 = null;
  for (const { path } of entries) {
    const name = path.slice(prefix.length);
    const file = await ctx.read(path);
    files[name] = file.source;
    if (/^q\d-\d{4}-report\.md$/.test(name)) reports.push({ name, ...parseReport(file.content) });
    else if (/^capital-call-/.test(name)) calls.push({ name, ...parseNotice(file.content) });
    else if (/^distribution-notice-/.test(name)) distributions.push({ name, ...parseNotice(file.content) });
    else if (/^k1-summary-/.test(name)) k1 = { name, ...parseK1(file.content) };
  }
  if (!reports.length) throw new Error(`No quarterly reports found under ${prefix}`);
  const order = (r) => r.quarter.slice(3) + r.quarter[1];
  reports.sort((a, b) => order(a).localeCompare(order(b)));

  const checks = checkFund({ reports, calls, distributions, k1 });
  const moved = reports.length > 1 ? whatMoved(reports.at(-2), reports.at(-1)) : null;
  const note = await askModel(reports.at(-1));

  // Which files each check looked at, so the brief can cite them.
  const reportName = (q) => reports.find((r) => r.quarter === q)?.name;
  const noticesIn = (list, q) => list.filter((n) => quarterOf(n.date) === q).map((n) => n.name);
  const previous = (q) => reports[reports.findIndex((r) => r.quarter === q) - 1]?.name;
  for (const c of checks) {
    const extra = { "calls-vs-notices": noticesIn(calls, c.quarter), "distributions-vs-notices": noticesIn(distributions, c.quarter), continuity: [previous(c.quarter)], "paid-in-roll": [previous(c.quarter)], "k1-tie": [k1?.name] }[c.id] ?? [];
    c.files = [...(c.id === "k1-capital" ? [] : [reportName(c.quarter)]), ...extra, ...(c.id === "k1-capital" ? [k1.name] : [])].filter(Boolean);
  }

  const failed = checks.filter((c) => !c.ok);
  const report = `# ${reports.at(-1).fundName}: ${moved ? `what moved, ${moved.from} to ${moved.to}` : reports.at(-1).quarter}\n\n${moved ? `Your NAV went from ${money(moved.lp.start)} to ${money(moved.lp.end)}.\n\n` : ""}${failed.length ? `Checks that failed:\n${failed.map((c) => `- ${c.quarter}: ${c.label}: ${c.detail}`).join("\n")}\n` : "All checks passed.\n"}${note ? `\n${note}\n` : ""}`;
  const saved = await ctx.save(`funds/briefs/${fund}-${task.id}.md`, report, `What moved in ${fund}`);

  const latest = reports.at(-1);
  return {
    summary: `${fund}: ${failed.length} of ${checks.length} checks failed`,
    sources: [...Object.values(files), saved],
    data: {
      fund,
      name: latest.fundName,
      latest: latest.quarter,
      reports: reports.map((r) => ({ name: r.name, quarter: r.quarter, fund: r.fund, lp: r.lp, position: r.position, movers: r.movers, othersChange: r.othersChange })),
      calls: calls.map(({ name, date, total, share, purpose }) => ({ name, date, total, share, purpose })),
      distributions: distributions.map(({ name, date, total, share }) => ({ name, date, total, share })),
      moved,
      checks,
      files,
      report: saved,
    },
  };
}
