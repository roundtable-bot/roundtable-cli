// Turns the fund-reader's results into a brief where every claim cites its source.
import { citations } from "./cite.mjs";
import { parseTables } from "./parse.mjs";

/** The fund folder names in funds/lp.md (the "Slug" column of its table). */
export function fundSlugs(lpMarkdown) {
  const table = parseTables(lpMarkdown).find((t) => t.header.includes("Slug"));
  if (!table) throw new Error('funds/lp.md has no table with a "Slug" column');
  return table.rows.map((r) => r.Slug);
}

const quarterOf = (date) => `Q${Math.ceil(Number(date.slice(5, 7)) / 3)} ${date.slice(0, 4)}`;
const n = (v) => (v < 0 ? `(${Math.abs(Math.round(v)).toLocaleString("en-US")})` : Math.round(v).toLocaleString("en-US"));
const signed = (v) => `${v < 0 ? "-" : "+"}$${Math.abs(Math.round(v)).toLocaleString("en-US")}`;
const mm = (v) => `${v < 0 ? "-" : ""}$${(Math.abs(v) / 1e6).toFixed(1)}M`;
const change = (v) => (v < 0 ? mm(v) : `+${mm(v)}`);
const x = (v) => `${v.toFixed(2)}x`;

/** results: the fund-reader's { summary, sources, data } for each fund. */
export function renderBrief(results) {
  const { claim, cite, table } = citations();
  const funds = results.map((r) => r.data);
  const src = (d, names) => names.map((name) => d.files[name]).filter(Boolean);
  const reportFiles = (d) => d.reports.slice(-2).map((r) => r.name);

  const start = funds.reduce((s, d) => s + d.moved.lp.start, 0);
  const end = funds.reduce((s, d) => s + d.moved.lp.end, 0);
  const failed = funds.flatMap((d) => d.checks.filter((c) => !c.ok).map((c) => ({ ...c, fund: d })));
  const total = funds.reduce((s, d) => s + d.checks.length, 0);
  const out = [
    `# What moved: ${funds[0].moved.from} to ${funds[0].moved.to}`,
    "",
    `> Synthetic data. Your capital accounts in ${funds.length} funds. Every claim cites the exact file version it came from ([S1] etc., listed under Sources).`,
    "",
    `- ${claim(`Your combined NAV went from ${mm(start)} to ${mm(end)} (${signed(end - start)}, ${(((end - start) / start) * 100).toFixed(1)}%).`, funds.flatMap((d) => src(d, [d.reports.at(-1).name])))}`,
    `- ${claim(`${failed.length} of ${total} cross-checks between the documents failed: ${failed.map((c) => `${c.fund.name.replace(/,? L\.P\.$/, "")}: ${c.label}`).join("; ") || "none"}.`, funds.flatMap((d) => src(d, reportFiles(d))))}`,
    "",
  ];

  for (const d of funds) {
    const m = d.moved;
    out.push(`## ${d.name}`, "", `- ${claim(`Your capital account went from $${n(m.lp.start)} to $${n(m.lp.end)} (${signed(m.lp.change)}); TVPI ${x(m.lp.tvpi[0])} to ${x(m.lp.tvpi[1])}, DPI ${x(m.lp.dpi[0])} to ${x(m.lp.dpi[1])}.`, src(d, reportFiles(d)))}`, "");

    out.push("### What moved in your capital account (USD)", "", "| Item | Amount | Source |", "|---|---|---|");
    const from = cite(src(d, [d.reports.at(-1).name]));
    out.push(`| Start of ${m.to} | ${n(m.lp.start)} | ${from} |`);
    for (const b of m.lp.bridge) out.push(`| ${b.label} | ${n(b.amount)} | ${from} |`);
    out.push(`| End of ${m.to} | ${n(m.lp.end)} | ${from} |`, "");

    out.push("### Biggest valuation changes (fund level)", "");
    for (const v of m.movers) out.push(`- ${claim(`${v.company}: ${change(v.change)} ($${(v.prior / 1e6).toFixed(1)}M to $${(v.current / 1e6).toFixed(1)}M). ${v.basis}`, src(d, [d.reports.at(-1).name]))}`);
    out.push("");

    out.push("### Capital calls and distributions", "");
    const inQuarter = (list) => list.filter((x) => quarterOf(x.date) === m.to);
    for (const c of inQuarter(d.calls)) out.push(`- ${c.date}: ${claim(`called $${n(c.share)} of your commitment ($${n(c.total)} from all investors).`, src(d, [c.name]))}`);
    for (const x of inQuarter(d.distributions)) out.push(`- ${x.date}: ${claim(`distributed $${n(x.share)} to you ($${n(x.total)} in total).`, src(d, [x.name]))}`);
    if (!inQuarter(d.calls).length && !inQuarter(d.distributions).length) out.push(`- ${claim("No capital calls or distributions.", src(d, [d.reports.at(-1).name]))}`);
    out.push("");

    out.push("### Cross-checks", "");
    for (const c of d.checks.filter((x) => !x.ok)) out.push(`- **Failed:** ${claim(`${c.label} (${c.quarter}): ${c.detail}`, src(d, c.files))}`);
    const passed = d.checks.filter((c) => c.ok);
    out.push(`- ${claim(`${passed.length} other checks passed, covering: ${[...new Set(passed.map((c) => c.label))].join("; ")}.`, src(d, reportFiles(d)))}`, "");
  }
  out.push(table(), "");
  return out.join("\n");
}

