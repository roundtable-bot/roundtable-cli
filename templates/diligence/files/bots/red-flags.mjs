// red-flags: reads a startup data room and reports inconsistencies.
//
// The rules below are pure functions (files in, flags out), so you can edit and test them
// without Roundtable. Each rule recomputes a number from the source files and compares it with
// what the deck claims. Add your own rule to RULES, or an LLM at the marked spot in handle().

export const id = "red-flags"; // matches the bot name in template.json
export const skills = [
  "Find inconsistencies between a deck and its financial model",
  "Recompute ARR, churn, burn and runway from source files",
];

const FILES = {
  deck: "deck.md",
  model: "financial-model.csv",
  cohorts: "cohorts.csv",
  hiring: "hiring-plan.csv",
  contracts: "contracts-summary.md",
};

// ---- parsing helpers ----------------------------------------------------------------------

const SCALE = { k: 1e3, m: 1e6, b: 1e9 };

/** "$4.8M" -> 4800000, "1,160,000" -> 1160000, "15 months" -> 15. */
export function parseNumber(text) {
  const m = /(\d[\d,]*(?:\.\d+)?)\s*([kmb])?\b/i.exec(String(text));
  return m ? Number(m[1].replaceAll(",", "")) * (SCALE[(m[2] ?? "").toLowerCase()] ?? 1) : null;
}

/** "7%" -> 0.07, "under 10%" -> 0.1. */
export function parsePercent(text) {
  const m = /(\d+(?:\.\d+)?)\s*%/.exec(String(text));
  return m ? Number(m[1]) / 100 : null;
}

function splitCsvLine(line) {
  const cells = [];
  let cell = "";
  let quoted = false;
  for (const ch of line) {
    if (ch === '"') quoted = !quoted;
    else if (ch === "," && !quoted) {
      cells.push(cell);
      cell = "";
    } else cell += ch;
  }
  return [...cells, cell];
}

/** CSV text -> array of objects keyed by the header row. */
export function parseCsv(text) {
  const [header, ...lines] = text.trim().split(/\r?\n/).map(splitCsvLine);
  return lines.map((cells) => Object.fromEntries(header.map((h, i) => [h.trim(), (cells[i] ?? "").trim()])));
}

/** Markdown text -> array of tables: { header: [...], rows: [{ column: cell }] }. */
export function parseTables(markdown) {
  const tables = [];
  let block = [];
  for (const line of [...markdown.split(/\r?\n/), ""]) {
    if (line.trim().startsWith("|")) block.push(line.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim()));
    else if (block.length) {
      const [header, , ...rows] = block;
      tables.push({ header, rows: rows.map((cells) => Object.fromEntries(header.map((h, i) => [h, cells[i] ?? ""]))) });
      block = [];
    }
  }
  return tables;
}

const usd = (n) => (n >= 1e6 ? `$${(n / 1e6).toFixed(2)}M` : `$${Math.round(n / 1e3).toLocaleString("en-US")}K`);
const pct = (x) => `${Math.round(x * 100)}%`;
const months = (n) => (Number.isFinite(n) ? n.toFixed(1) : "unlimited");
const sum = (xs) => xs.reduce((a, b) => a + b, 0);

function num(value, what) {
  const x = Number(value);
  if (!Number.isFinite(x)) throw new Error(`"${value}" is not a number (${what})`);
  return x;
}

function money(text, what) {
  const x = parseNumber(text);
  if (x == null) throw new Error(`"${text}" is not an amount (${what})`);
  return x;
}

function column(row, name) {
  if (row[name] === undefined) throw new Error(`missing column "${name}"`);
  return row[name];
}

/** First row of any table in the deck whose first cell matches `label`. */
function deckMetric(deck, label) {
  for (const t of parseTables(deck)) {
    const row = t.rows.find((r) => label.test(r[t.header[0]]));
    if (row) return { label: row[t.header[0]], value: row[t.header[1]] };
  }
  return null;
}

const deckQuote = (m) => ({ file: FILES.deck, quote: `${m.label}: ${m.value}` });

function modelRows(room) {
  return parseCsv(room[FILES.model]).map((r) => ({
    month: r.month,
    type: r.type,
    sub: num(r.subscription_revenue, "subscription_revenue"),
    fees: num(r.implementation_fees, "implementation_fees"),
    cogs: num(r.cogs, "cogs"),
    payroll: num(r.opex_payroll, "opex_payroll"),
    burn: num(r.net_burn, "net_burn"),
    cash: num(r.ending_cash, "ending_cash"),
    headcount: num(r.headcount, "headcount"),
  }));
}

/** The last three actual months: the "current quarter" for margin and burn comparisons. */
const lastQuarter = (rows) => rows.filter((r) => r.type === "actual").slice(-3);

/** Months of cash left, burning `burns` one month at a time and then `tail` a month forever. */
function runwayMonths(cash, burns, tail) {
  let left = cash;
  for (const [i, b] of burns.entries()) {
    if (left <= b) return i + left / b;
    left -= b;
  }
  return tail > 0 ? burns.length + left / tail : Infinity;
}

function addMonths(month, n) {
  const [y, m] = month.split("-").map(Number);
  const d = new Date(Date.UTC(y, m - 1 + n, 1));
  return d.toLocaleString("en-US", { month: "short", year: "numeric", timeZone: "UTC" });
}

// ---- rules --------------------------------------------------------------------------------
// A rule returns { flag?, metrics? }. `needs` lists the files it cannot run without.

const arrIncludesOneTime = {
  id: "arr-includes-one-time",
  needs: [FILES.deck, FILES.model],
  run(room) {
    const claim = deckMetric(room[FILES.deck], /^ARR\b/i);
    if (!claim) throw new Error("no ARR row in the deck's metrics table");
    const last = modelRows(room).filter((r) => r.type === "actual").at(-1);
    const deckArr = parseNumber(claim.value);
    const recurring = 12 * last.sub;
    const total = 12 * (last.sub + last.fees);
    const metrics = [
      { label: "ARR", deck: usd(deckArr), actual: `${usd(recurring)} recurring`, match: deckArr <= recurring * 1.05, files: [FILES.deck, FILES.model] },
    ];
    if (deckArr <= recurring * 1.05) return { metrics };
    const detail = [
      `The deck reports ARR of ${usd(deckArr)}. Recurring subscription revenue in ${last.month} was ${usd(last.sub)} a month, which annualises to ${usd(recurring)}.`,
      Math.abs(deckArr - total) / total <= 0.02 && `The deck figure equals 12 times total revenue, which includes ${usd(last.fees)} of one-time implementation fees.`,
      `ARR is overstated by ${usd(deckArr - recurring)} (${pct((deckArr - recurring) / deckArr)} of the reported figure).`,
    ];
    return {
      metrics,
      flag: {
        id: this.id,
        severity: "high",
        title: "ARR includes one-time implementation fees",
        detail: detail.filter(Boolean).join(" "),
        evidence: [
          deckQuote(claim),
          { file: FILES.model, quote: `${last.month}: subscription_revenue ${last.sub}, implementation_fees ${last.fees}` },
        ],
        question: "Restate ARR from recurring subscription revenue only. What is the figure on that basis, and how does the growth rate change?",
      },
    };
  },
};

const cohortChurnHidden = {
  id: "cohort-churn-hidden",
  needs: [FILES.cohorts],
  run(room) {
    const byCohort = new Map();
    for (const r of parseCsv(room[FILES.cohorts])) {
      const ages = byCohort.get(r.cohort) ?? {};
      ages[num(r.age_months, "age_months")] = { logos: num(r.active_logos, "active_logos"), mrr: num(r.mrr_usd, "mrr_usd") };
      byCohort.set(r.cohort, ages);
    }
    const mature = [...byCohort].filter(([, a]) => a[0] && a[12]).sort(([x], [y]) => x.localeCompare(y));
    if (!mature.length) throw new Error("no cohort is 12 months old yet");

    const retention = (cohorts) => ({
      logos: sum(cohorts.map(([, a]) => a[12].logos)) / sum(cohorts.map(([, a]) => a[0].logos)),
      dollars: sum(cohorts.map(([, a]) => a[12].mrr)) / sum(cohorts.map(([, a]) => a[0].mrr)),
    });
    const all = retention(mature);
    const churn = 1 - all.logos;

    const deck = room[FILES.deck];
    const claimedChurn = deck && deckMetric(deck, /logo churn/i);
    const claimed = claimedChurn && parsePercent(claimedChurn.value);
    const claimedNrr = deck && deckMetric(deck, /net revenue retention/i);
    const fire = claimed != null ? churn - claimed >= 0.1 : churn >= 0.3;

    const metrics = [];
    if (claimed != null) metrics.push({ label: "Annual logo churn", deck: pct(claimed), actual: `${pct(churn)} (cohorts)`, match: !fire, files: [FILES.deck, FILES.cohorts] });
    if (claimedNrr) metrics.push({ label: "Net revenue retention", deck: pct(parsePercent(claimedNrr.value)), actual: `${pct(all.dollars)} (cohorts)`, match: Math.abs(parsePercent(claimedNrr.value) - all.dollars) < 0.03, files: [FILES.deck, FILES.cohorts] });
    if (!fire) return { metrics };

    // Which cohort carries the dollar retention, and is churn getting worse?
    const detail = [
      `Of the ${mature.length} cohorts old enough to measure, ${pct(all.logos)} of logos were still active after 12 months: ${pct(churn)} annual logo churn${claimed != null ? `, against ${pct(claimed)} in the deck` : ""}.`,
      `Dollar retention over the same cohorts is ${pct(all.dollars)}${all.dollars - all.logos >= 0.25 ? ", so expansion by a few accounts is hiding the loss of customers" : ""}.`,
    ];
    const best = mature.map((c) => ({ cohort: c[0], ...retention([c]) })).sort((a, b) => b.dollars - a.dollars)[0];
    const rest = retention(mature.filter(([c]) => c !== best.cohort));
    if (mature.length > 2 && best.dollars >= 1.5 && all.dollars - rest.dollars >= 0.1) {
      detail.push(`The ${best.cohort} cohort alone retains ${pct(best.dollars)} of its revenue; without it dollar retention is ${pct(rest.dollars)}.`);
    }
    const half = Math.floor(mature.length / 2);
    const older = mature.length >= 4 && retention(mature.slice(0, half));
    const newer = mature.length >= 4 && retention(mature.slice(-half));
    if (older && older.logos - newer.logos >= 0.04) {
      detail.push(`12-month logo retention slipped from ${pct(older.logos)} (older cohorts) to ${pct(newer.logos)} (newer cohorts).`);
    }
    return {
      metrics,
      flag: {
        id: this.id,
        severity: "high",
        title: all.dollars - all.logos >= 0.25 ? "Customer churn is hidden behind dollar retention" : "Logo churn is higher than the deck says",
        detail: detail.join(" "),
        evidence: [
          ...(claimedChurn ? [deckQuote(claimedChurn)] : []),
          { file: FILES.cohorts, quote: `12-month logo retention ${pct(all.logos)}, dollar retention ${pct(all.dollars)} across ${mature.length} cohorts` },
        ],
        question: "Provide logo and revenue retention by cohort excluding the largest account, and the reason for each churned customer in the last 12 months.",
      },
    };
  },
};

const burnVsHiringPlan = {
  id: "burn-vs-hiring-plan",
  needs: [FILES.model, FILES.hiring],
  run(room) {
    const rows = modelRows(room);
    const last = rows.filter((r) => r.type === "actual").at(-1);
    const forecast = rows.filter((r) => r.type === "forecast");
    if (!forecast.length) throw new Error("the model has no forecast months");
    const hires = parseCsv(room[FILES.hiring]).map((h) => ({ start: column(h, "start_month"), cost: num(column(h, "loaded_monthly_cost_usd"), "loaded_monthly_cost_usd") }));

    let planTotal = 0;
    let modelTotal = 0;
    const adjusted = forecast.map((m) => {
      const plan = sum(hires.filter((h) => h.start <= m.month).map((h) => h.cost));
      const uplift = m.payroll - last.payroll;
      planTotal += plan;
      modelTotal += uplift;
      return m.burn + plan - uplift;
    });
    const modelRunway = runwayMonths(last.cash, forecast.map((m) => m.burn), forecast.at(-1).burn);
    const planRunway = runwayMonths(last.cash, adjusted, adjusted.at(-1));
    const deckRunway = room[FILES.deck] && deckMetric(room[FILES.deck], /^runway/i);
    const metrics = [];
    const fire = planTotal - modelTotal * 1.25 > 100_000;
    if (deckRunway) {
      metrics.push({ label: "Runway", deck: deckRunway.value, actual: `${months(planRunway)} months with the hiring plan`, match: !fire, files: [FILES.deck, FILES.model, FILES.hiring] });
    }
    if (!fire) return { metrics };

    const lastForecast = forecast.at(-1);
    return {
      metrics,
      flag: {
        id: this.id,
        severity: "high",
        title: "Burn and runway ignore the hiring plan",
        detail: [
          `The hiring plan adds ${hires.length} people at a loaded cost of ${usd(planTotal)} over the next ${forecast.length} months, but the model's payroll line rises by only ${usd(modelTotal)} and headcount goes from ${last.headcount} to ${lastForecast.headcount}.`,
          `With the missing payroll added, cash lasts ${months(planRunway)} months from ${last.month}${Number.isFinite(planRunway) ? ` (to about ${addMonths(last.month, Math.floor(planRunway))})` : ""}, not the ${months(modelRunway)} months the model implies${deckRunway ? ` or the ${deckRunway.value} in the deck` : ""}.`,
        ].join(" "),
        evidence: [
          { file: FILES.hiring, quote: `${hires.length} hires, ${usd(planTotal)} loaded cost over the forecast months` },
          { file: FILES.model, quote: `opex_payroll ${last.payroll} -> ${lastForecast.payroll}, headcount ${last.headcount} -> ${lastForecast.headcount} (${forecast[0].month} to ${lastForecast.month})` },
          ...(deckRunway ? [deckQuote(deckRunway)] : []),
        ],
        question: "Rebuild the forecast with the hiring plan included, with and without the Series A proceeds. When does the company need to close the round?",
      },
    };
  },
};

const deckModelMismatch = {
  id: "deck-model-mismatch",
  needs: [FILES.deck, FILES.model],
  run(room) {
    const deck = room[FILES.deck];
    const rows = modelRows(room);
    const q = lastQuarter(rows);
    const checks = [];
    const check = (label, claim, deckValue, actual, show, tolerance, files) => {
      if (!claim || deckValue == null) return;
      const gap = Math.abs(deckValue - actual) / (tolerance.relative ? actual : 1);
      checks.push({ label, claim, deckValue, actual, gap, ok: gap < tolerance.limit, show, files });
    };

    const margin = 1 - sum(q.map((r) => r.cogs)) / sum(q.map((r) => r.sub + r.fees));
    const gm = deckMetric(deck, /gross margin/i);
    check("Gross margin", gm, gm && parsePercent(gm.value), margin, pct, { limit: 0.03 }, [FILES.deck, FILES.model]);

    const burn = sum(q.map((r) => r.burn)) / q.length;
    const nb = deckMetric(deck, /net burn/i);
    check("Net burn", nb, nb && parseNumber(nb.value), burn, usd, { relative: true, limit: 0.1 }, [FILES.deck, FILES.model]);

    const cash = deckMetric(deck, /^cash/i);
    check("Cash", cash, cash && parseNumber(cash.value), q.at(-1).cash, usd, { relative: true, limit: 0.03 }, [FILES.deck, FILES.model]);

    if (room[FILES.cohorts]) {
      const latest = new Map();
      for (const r of parseCsv(room[FILES.cohorts])) {
        const age = num(r.age_months, "age_months");
        if (!latest.has(r.cohort) || latest.get(r.cohort).age < age) latest.set(r.cohort, { age, logos: num(r.active_logos, "active_logos") });
      }
      const active = sum([...latest.values()].map((c) => c.logos));
      const customers = deckMetric(deck, /^customers/i);
      check("Customers", customers, customers && parseNumber(customers.value), active, (n) => `${n}`, { relative: true, limit: 0.05 }, [FILES.deck, FILES.cohorts]);
    }

    const metrics = checks.map((c) => ({ label: c.label, deck: c.claim.value, actual: c.show(c.actual), match: c.ok, files: c.files }));
    const wrong = checks.filter((c) => !c.ok);
    if (!wrong.length) return { metrics };
    return {
      metrics,
      flag: {
        id: this.id,
        severity: "medium",
        title: "Deck numbers disagree with the model",
        detail: `${wrong.map((c) => `${c.label}: the deck says ${c.claim.value}, the data room shows ${c.show(c.actual)}.`).join(" ")} Each figure was recomputed from the files in this room.`,
        evidence: wrong.flatMap((c) => [deckQuote(c.claim), { file: c.files[1], quote: `${c.label} recomputed: ${c.show(c.actual)}` }]),
        question: `Reconcile ${wrong.map((c) => c.label.toLowerCase()).join(" and ")} between the deck and the model, and state how each is defined.`,
      },
    };
  },
};

/** Rows of the contract table with ACV, term end, auto-renew and termination notice parsed. */
function contractRows(room) {
  const table = parseTables(room[FILES.contracts]).find((t) => t.header[0] === "Customer");
  if (!table) throw new Error('no contract table (first column "Customer")');
  return table.rows.map((r) => ({
    customer: r.Customer,
    acv: money(column(r, "ACV (USD)"), "ACV (USD)"),
    termEnd: column(r, "Term end"),
    autoRenew: /^yes/i.test(column(r, "Auto-renew")),
    noticeDays: Number(/(\d+)\s*days/i.exec(column(r, "Termination for convenience"))?.[1]) || null,
  }));
}

function recurringArr(room) {
  return 12 * modelRows(room).filter((r) => r.type === "actual").at(-1).sub;
}

const customerConcentration = {
  id: "customer-concentration",
  needs: [FILES.contracts, FILES.model],
  run(room) {
    const arr = recurringArr(room);
    const top = contractRows(room).sort((a, b) => b.acv - a.acv)[0];
    const share = top.acv / arr;
    const claim = room[FILES.deck] && deckMetric(room[FILES.deck], /largest customer/i);
    const claimed = claim && parsePercent(claim.value);
    const fire = share >= 0.2 || (claimed != null && share > claimed + 0.05);
    const metrics = claim
      ? [{ label: "Largest customer, share of ARR", deck: claim.value, actual: `${pct(share)} (${top.customer})`, match: !fire, files: [FILES.deck, FILES.contracts] }]
      : [];
    if (!fire) return { metrics };
    return {
      metrics,
      flag: {
        id: this.id,
        severity: "high",
        title: `Largest customer is ${pct(share)} of recurring ARR`,
        detail: `${top.customer} pays ${usd(top.acv)} a year, which is ${pct(share)} of recurring ARR (${usd(arr)})${claim ? `, while the deck says the largest customer is ${claim.value}` : ""}. Losing it would cut recurring revenue by ${pct(share)}.`,
        evidence: [...(claim ? [deckQuote(claim)] : []), { file: FILES.contracts, quote: `${top.customer}: ACV ${top.acv.toLocaleString("en-US")}` }],
        question: "What are the renewal status and pricing discussions for the largest customer? Provide the contract and any correspondence about price or scope.",
      },
    };
  },
};

const renewalCliff = {
  id: "renewal-cliff",
  needs: [FILES.contracts, FILES.model],
  run(room) {
    const arr = recurringArr(room);
    const [y, m] = modelRows(room).filter((r) => r.type === "actual").at(-1).month.split("-").map(Number);
    const asOf = Date.UTC(y, m, 0); // last day of the last actual month
    const atRisk = contractRows(room)
      .map((c) => ({ ...c, days: Math.round((Date.parse(c.termEnd) - asOf) / 864e5) }))
      .filter((c) => (c.days <= 180 && !c.autoRenew) || (c.noticeDays != null && c.noticeDays <= 60));
    const share = sum(atRisk.map((c) => c.acv)) / arr;
    if (share < 0.25) return {};
    const why = (c) => {
      const reasons = [];
      if (c.days <= 180 && !c.autoRenew) reasons.push(`ends ${c.termEnd} with no auto-renewal`);
      if (c.noticeDays != null && c.noticeDays <= 60) reasons.push(`can be terminated at ${c.noticeDays} days' notice`);
      return `${c.customer} (${usd(c.acv)}) ${reasons.join(" and ")}`;
    };
    return {
      flag: {
        id: this.id,
        severity: "medium",
        title: "Large contracts can end within six months",
        detail: `${pct(share)} of recurring ARR sits in contracts that end within 180 days without auto-renewal, or that can be terminated at 60 days' notice or less: ${atRisk.map(why).join("; ")}.`,
        evidence: atRisk.map((c) => ({ file: FILES.contracts, quote: `${c.customer}: term end ${c.termEnd}, auto-renew ${c.autoRenew ? "yes" : "no"}` })),
        question: "Provide the status of each of these renewals and the pricing being discussed. Which of them are already in the forecast at full price?",
      },
    };
  },
};

export const RULES = [arrIncludesOneTime, cohortChurnHidden, burnVsHiringPlan, deckModelMismatch, customerConcentration, renewalCliff];

const SEVERITY = { high: 0, medium: 1, low: 2 };

/**
 * room: { "deck.md": "...", "financial-model.csv": "...", ... } (file name -> text)
 * Returns { flags, metrics, skipped }. `skipped` lists rules that could not run and why, so a
 * missing or reshaped file never looks like a clean bill of health.
 */
export function findRedFlags(room) {
  const flags = [];
  const metrics = [];
  const skipped = [];
  for (const rule of RULES) {
    const missing = rule.needs.filter((f) => !room[f]);
    if (missing.length) {
      skipped.push({ rule: rule.id, reason: `missing ${missing.join(", ")}` });
      continue;
    }
    try {
      const out = rule.run(room);
      if (out.flag) flags.push(out.flag);
      metrics.push(...(out.metrics ?? []));
    } catch (err) {
      skipped.push({ rule: rule.id, reason: `could not read the files: ${err.message}` });
    }
  }
  return { flags: flags.sort((a, b) => SEVERITY[a.severity] - SEVERITY[b.severity]), metrics, skipped };
}

// ---- LLM hook -----------------------------------------------------------------------------
// Rules only catch what we thought to check. To catch the rest, send `room` to a model here
// and return extra flags in the same shape as the rules above:
//   { id, severity: "high" | "medium" | "low", title, detail, question,
//     evidence: [{ file: "deck.md", quote: "..." }] }
// Keep `evidence.file` set to a file name from the room: it becomes the citation in the memo.
// Put your API key in .env (it is loaded by `npm run dev`) and never commit it.
async function askModel(room, ruleFlags) {
  return [];
}

// ---- the bot ------------------------------------------------------------------------------

const MAX_FILES = 100;

function renderReport(title, flags) {
  const items = flags.map((f, i) => `## ${i + 1}. ${f.title} (${f.severity})\n\n${f.detail}\n\n${f.evidence.map((e) => `- \`${e.file}\`: ${e.quote}`).join("\n")}\n\nQuestion: ${f.question}\n`);
  return `# ${title}\n\n${flags.length} red flags.\n\n${items.join("\n")}`;
}

export async function handle(task, ctx) {
  const prefix = (task.input?.prefix ?? "dataroom/acme/").replace(/\/?$/, "/");
  const entries = (await ctx.list(prefix)).slice(0, MAX_FILES);
  const room = {};
  const sources = {};
  for (const { path } of entries) {
    const file = await ctx.read(path);
    const name = path.slice(prefix.length);
    room[name] = file.content;
    sources[name] = file.source;
  }
  await ctx.progress(`Read ${entries.length} files; checking`);

  const { flags: ruleFlags, metrics, skipped } = findRedFlags(room);
  const flags = [...ruleFlags, ...(await askModel(room, ruleFlags))];

  // Attach each flag's citations: the exact file versions it was computed from.
  const cite = (files) => [...new Set(files)].map((f) => sources[f]).filter(Boolean);
  const withSources = flags.map((f) => ({ ...f, sources: cite(f.evidence.map((e) => e.file)) }));
  const metricsWithSources = metrics.map((m) => ({ ...m, sources: cite(m.files) }));

  const saved = await ctx.save(`flags/${task.id}.md`, renderReport("Red flags for Acme Ledger", withSources), "Red flags for Acme Ledger");
  return {
    summary: `${flags.length} red flags (${flags.filter((f) => f.severity === "high").length} high)`,
    sources: [...Object.values(sources), saved],
    data: { flags: withSources, metrics: metricsWithSources, skipped, report: saved },
  };
}
