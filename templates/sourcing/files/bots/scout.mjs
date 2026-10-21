// scout: ranks founders from public signals and explains why each one deserves a meeting.
//
// The scoring is plain rules (see scoreFounder) that mirror sourcing/thesis.md in the bank:
// edit the weights here, and the thesis there. The rules are pure functions, so you can try
// them without Roundtable. An LLM can add judgement at the marked spot in handle().

import { field, parseNumber, parseTables } from "../lib/parse.mjs";

export const id = "scout";
export const skills = ["Rank founders by public signals", "Explain why a founder deserves a meeting"];

const DAY = 864e5;
const days = (from, to) => Math.round((Date.parse(to) - Date.parse(from)) / DAY);

// ---- parsing ------------------------------------------------------------------------------

export function parseProfile(markdown) {
  const building = field(markdown, "Building") ?? "";
  const signals = parseTables(markdown).find((t) => t.header[0] === "Date")?.rows ?? [];
  return {
    name: /^# (.+)$/m.exec(markdown)?.[1].trim(),
    location: field(markdown, "Location"),
    stage: field(markdown, "Stage"),
    company: building.split(":")[0].trim(),
    description: building.split(":").slice(1).join(":").trim(),
    signals: signals.map((r) => ({ date: r.Date, type: r.Type, text: r.Signal, source: r.Source })),
  };
}

export function parseFiling(markdown) {
  const people = parseTables(markdown).find((t) => t.header[0] === "Name")?.rows ?? [];
  return {
    issuer: field(markdown, "Issuer"),
    filed: field(markdown, "Filed"),
    offering: parseNumber(field(markdown, "Total offering amount")),
    sold: parseNumber(field(markdown, "Total amount sold")),
    investors: parseNumber(field(markdown, "Investors")),
    people: people.map((p) => p.Name),
  };
}

// ---- scoring ------------------------------------------------------------------------------

/** Points for one signal before the recency discount, and what made it count. */
function weigh(s) {
  const n = (re) => parseNumber(re.exec(s.text)?.[1]);
  switch (s.type) {
    case "shipped":
      // Shipped product with real use. Paying customers count extra.
      return 3 + (n(/([\d,]+) weekly active/) >= 1000 ? 1 : 0) + (/paying customers/.test(s.text) ? 1 : 0);
    case "repo": {
      if (/curated list|tutorial|fork/i.test(s.text)) return 0; // stars on lists and forks say little
      const commits = n(/(\d+) commits/) ?? 0;
      return (commits >= 150 ? 2 : commits >= 50 ? 1 : 0) + (n(/([\d,]+) stars/) >= 500 ? 1 : 0);
    }
    case "hackathon": {
      const place = /^(1st|2nd|3rd)/.exec(s.text)?.[1];
      return place ? (place === "1st" ? 2 : 1) + (n(/(\d+) teams/) >= 100 ? 1 : 0) : 0;
    }
    case "paper":
      return /first author/i.test(s.text) ? 1 : 0;
    case "job_change":
      // Leaving a job to build full time. Joining another company is not a signal.
      return /^Left .* to (work on|start|build) .* full time/i.test(s.text) ? 2 : 0;
    default:
      return 0;
  }
}

const recency = (age) => (age <= 90 ? 1 : age <= 365 ? 0.5 : 0.2);

/**
 * profile: from parseProfile. filings: array from parseFiling. today: "YYYY-MM-DD".
 * Returns { score, evidence, timing, filing }; evidence is sorted by points, best first.
 */
export function scoreFounder(profile, filings, today) {
  const evidence = profile.signals
    .map((s) => ({ ...s, age: days(s.date, today), points: weigh(s) }))
    .map((s) => ({ ...s, points: s.points * recency(s.age) }))
    .filter((s) => s.points > 0)
    .sort((a, b) => b.points - a.points);

  // Different kinds of signal in the last six months, from independent places, point the same way.
  const kinds = new Set(evidence.filter((s) => s.age <= 180).map((s) => s.type));
  const convergence = kinds.size >= 4 ? 3 : kinds.size === 3 ? 2 : 0;

  // A Form D that lists the founder tells us about funding. Still raising is a reason to meet now;
  // a large round already closed means other investors got there first.
  const filing = filings.find((f) => f.people.includes(profile.name)) ?? null;
  let timing = { points: 0, text: `No Form D on file; ${profile.company} is at ${profile.stage} stage.` };
  if (filing) {
    const raising = filing.sold < filing.offering && days(filing.filed, today) <= 120;
    const sold = `$${(filing.sold / 1e6).toFixed(2)}M`;
    if (filing.sold >= 5e6) timing = { points: -3, text: `Form D filed ${filing.filed}: ${sold} already sold, so the round is likely led.` };
    else if (raising) timing = { points: 1, text: `Form D filed ${filing.filed}: ${sold} of $${(filing.offering / 1e6).toFixed(2)}M sold, so the round is open now.` };
    else timing = { points: 0, text: `Form D filed ${filing.filed}: ${sold} sold of a $${(filing.offering / 1e6).toFixed(2)}M offering.` };
  }

  const score = evidence.reduce((sum, s) => sum + s.points, 0) + convergence + timing.points;
  return { score: Math.round(score * 10) / 10, evidence, convergence: kinds.size >= 3 ? kinds.size : 0, timing, filing };
}

/** Ranks every profile. `filings` is the parsed Form D list. */
export function rankFounders(profiles, filings, today) {
  return profiles
    .map((profile) => ({ profile, ...scoreFounder(profile, filings, today) }))
    .sort((a, b) => b.score - a.score || a.profile.name.localeCompare(b.profile.name));
}

/** One paragraph on why this founder deserves a meeting, built only from the evidence. */
export function whyMeet({ profile, evidence, convergence, timing }) {
  const best = evidence.slice(0, 2).map((s) => s.text.replace(/\.$/, ""));
  const parts = [`${profile.name} is building ${profile.company} (${profile.description}).`];
  if (best.length) parts.push(`Strongest signals: ${best.join("; ")}.`);
  if (convergence) parts.push(`${convergence} different kinds of public signal in the last six months point the same way.`);
  parts.push(timing.text);
  return parts.join(" ");
}

// ---- LLM hook -----------------------------------------------------------------------------
// The rules only count signals. To add judgement (is this repo actually good? does the paper
// matter for the product?), send each pick to a model here and return a sentence to add to its
// "why". Put your API key in .env and never commit it.
async function askModel(pick) {
  return null; // e.g. "The 0.4 release added multi-agent trace replay, which competitors lack."
}

// ---- the bot ------------------------------------------------------------------------------

export async function handle(task, ctx) {
  const prefix = (task.input?.prefix ?? "sourcing/").replace(/\/?$/, "/");
  const top = Math.min(Math.max(Number(task.input?.top) || 5, 1), 20);
  const entries = await ctx.list(prefix);
  const read = async (paths) => Promise.all(paths.map((path) => ctx.read(path)));

  const thesis = entries.find((e) => e.path === `${prefix}thesis.md`);
  const thesisFile = thesis && (await ctx.read(thesis.path));
  // Signals are scored relative to the data's date, so the demo gives the same answer any day.
  const today = task.input?.today ?? (thesisFile && /\*\*Data as of:\*\* (\d{4}-\d{2}-\d{2})/.exec(thesisFile.content)?.[1]) ?? new Date().toISOString().slice(0, 10);

  const profilePaths = entries.map((e) => e.path).filter((p) => p.startsWith(`${prefix}profiles/`));
  const filingPaths = entries.map((e) => e.path).filter((p) => p.startsWith(`${prefix}filings/`));
  await ctx.progress(`Reading ${profilePaths.length} profiles and ${filingPaths.length} filings`);
  const profileFiles = await read(profilePaths);
  const filingFiles = await read(filingPaths);

  const filings = filingFiles.map((f) => ({ ...parseFiling(f.content), source: f.source }));
  const ranked = rankFounders(profileFiles.map((f) => ({ ...parseProfile(f.content), source: f.source })), filings, today);

  const picks = [];
  for (const r of ranked.slice(0, top)) {
    const extra = await askModel(r);
    picks.push({
      name: r.profile.name,
      company: r.profile.company,
      score: r.score,
      why: [whyMeet(r), extra].filter(Boolean).join(" "),
      evidence: r.evidence.slice(0, 4).map((s) => ({ date: s.date, type: s.type, text: s.text })),
      timing: r.timing.text,
      // Citations: the profile the signals came from, and the filing if one was used.
      sources: [r.profile.source, ...(r.filing ? [r.filing.source] : [])],
    });
  }

  const report = `# Founder shortlist (${today})\n\n${picks.map((p, i) => `${i + 1}. **${p.name}**, ${p.company}: score ${p.score}. ${p.why}`).join("\n")}\n`;
  const saved = await ctx.save(`sourcing/shortlists/${task.id}.md`, report, "Founder shortlist");
  return {
    summary: `Top ${picks.length} of ${ranked.length} founders`,
    sources: [saved, ...(thesisFile ? [thesisFile.source] : [])],
    data: { today, scanned: ranked.length, picks, report: saved },
  };
}
