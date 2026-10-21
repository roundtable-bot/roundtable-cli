// Turns the bots' results into a memo in which every claim carries a citation.
// A citation is [S1], [S2], ...; the Sources table at the end maps each one to the exact
// bank, path and file version the bot read. Pure functions: no SDK, no network.

const money = (n) => (n >= 1e9 ? `$${(n / 1e9).toFixed(2)}B` : `$${Math.round(n / 1e6)}M`);
const median = (xs) => {
  const s = xs.filter((x) => x != null).sort((a, b) => a - b);
  return s.length ? (s.length % 2 ? s[(s.length - 1) / 2] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2) : null;
};

/**
 * flags, comps, market: gather() results, { status: "done" | "blocked", result }.
 * A "done" result is the bot's { summary, sources, data }; a "blocked" one is { error }.
 */
export function renderMemo({ company, generatedOn, flags, comps, market }) {
  const sources = [];
  const cite = (s) => {
    let i = sources.findIndex((x) => x.bank === s.bank && x.path === s.path && x.version === s.version);
    if (i < 0) i = sources.push(s) - 1;
    return `[S${i + 1}]`;
  };
  // Every claim goes through here. A claim without a source is a bug, so it throws.
  const claim = (text, from) => {
    if (!from?.length) throw new Error(`Claim without a source: ${text}. Set each flag's evidence.file to a file name from the data room.`);
    return `${text} ${from.map(cite).join("")}`;
  };
  const failed = (name, r) => `> The ${name} bot did not finish${r?.result?.error ? `: ${r.result.error}` : ""}. This section is empty.`;
  const done = (r) => r?.status === "done";
  const out = [];

  out.push(
    `# ${company}: first-draft diligence memo`,
    "",
    `> Synthetic data room. Draft generated ${generatedOn} by Roundtable bots. Every claim cites the exact file version it came from ([S1] etc., listed under Sources). An analyst must review it before it is shared.`,
    "",
    "## Summary",
    "",
  );

  const flagData = done(flags) && flags.result.data;
  if (flagData) {
    const high = flagData.flags.filter((f) => f.severity === "high").length;
    out.push(`- ${claim(`${flagData.flags.length} red flags, ${high} of them high severity: ${flagData.flags.map((f) => f.title).join("; ")}.`, [flagData.report])}`);
    const arr = flagData.metrics.find((m) => m.label === "ARR" && !m.match);
    if (arr) out.push(`- ${claim(`The deck reports ARR of ${arr.deck}; the data room's own numbers give ${arr.actual}.`, arr.sources)}`);
    out.push(`- ${claim("Draft view, for the analyst to confirm: hold the investment decision until management answers the questions at the end of this memo.", [flagData.report])}`);
  }
  const marketData = done(market) && market.result.data;
  if (marketData) out.push(`- ${claim(`Market (${marketData.source} data): ${marketData.signals[0].text}`, [marketData.report])}`);
  const compsData = done(comps) && comps.result.data;
  if (compsData) {
    const growth = median(compsData.comps.map((c) => c.growthPct));
    const margin = median(compsData.comps.map((c) => c.grossMarginPct));
    out.push(`- ${claim(`Comparable public companies (${compsData.source} data): median revenue growth ${growth ?? "n/a"}%, median gross margin ${margin ?? "n/a"}%.`, [compsData.report])}`);
  }
  if (!flagData) out.push(failed("red-flags", flags));

  out.push("", "## Deck versus data room", "");
  if (flagData) {
    out.push("| Metric | Deck says | Data room shows | Match | Sources |", "|---|---|---|---|---|");
    for (const m of flagData.metrics) out.push(`| ${m.label} | ${m.deck} | ${m.actual} | ${m.match ? "yes" : "**no**"} | ${m.sources.map(cite).join("")} |`);
    if (!flagData.metrics.length) out.push("| (no metrics could be compared) | | | | |");
  }

  out.push("", "## Red flags", "");
  if (flagData) {
    flagData.flags.forEach((f, i) => {
      out.push(`### ${i + 1}. ${f.title} (${f.severity})`, "", `- ${claim(f.detail, f.sources)}`);
      for (const e of f.evidence) {
        const from = f.sources.filter((s) => s.path.endsWith(`/${e.file}`));
        out.push(`- Evidence in \`${e.file}\`: ${claim(`"${e.quote}"`, from.length ? from : f.sources)}`);
      }
      out.push(`- ${claim(`Question for management: ${f.question}`, f.sources)}`, "");
    });
    if (!flagData.flags.length) out.push("- No red flags found by the rules.");
    for (const s of flagData.skipped) out.push(`> Rule \`${s.rule}\` did not run: ${s.reason}.`);
  } else out.push(failed("red-flags", flags));

  out.push("", "## Market signals", "");
  if (marketData) {
    out.push(`> ${marketData.source === "synthetic" ? "SYNTHETIC DATA. " : ""}${marketData.note}`, "");
    for (const s of marketData.signals) out.push(`- **${s.topic}** (${s.asOf}, ${s.label}): ${claim(s.text, [marketData.report])}`);
  } else out.push(failed("market", market));

  out.push("", "## Comparable public companies", "");
  if (compsData) {
    out.push(`> ${compsData.source === "sample" ? "SAMPLE DATA. " : ""}${compsData.note}`, "");
    out.push("| Company | Fiscal year end | Revenue | Growth | Gross margin | Source |", "|---|---|---|---|---|---|");
    for (const c of compsData.comps) {
      out.push(`| ${c.name} (${c.ticker}) | ${c.fiscalYearEnd} | ${money(c.revenueUsd)} | ${c.growthPct ?? "n/a"}% | ${c.grossMarginPct ?? "n/a"}% | ${cite(compsData.report)}${c.filing ? ` [filing](${c.filing})` : ""} |`);
    }
  } else out.push(failed("comps", comps));

  out.push("", "## Questions for management", "");
  if (flagData) flagData.flags.forEach((f, i) => out.push(`${i + 1}. ${claim(f.question, f.sources)}`));

  out.push("", "## Sources", "", "Version is the 12-character id of the file version the bot read. Open it in the bank's History to see exactly what it saw.", "", "| Ref | Bank | Path | Version |", "|---|---|---|---|");
  sources.forEach((s, i) => out.push(`| S${i + 1} | ${s.bank} | ${s.path} | ${s.version} |`));
  return out.join("\n") + "\n";
}
