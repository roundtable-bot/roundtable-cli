// Turns the watcher's findings into a digest where every claim cites its source.
import { citations } from "./cite.mjs";

// What a partner can do about each kind of signal.
const NEXT_STEP = {
  runway: "Ask for a 13-week cash plan and the status of any raise.",
  revenue: "Ask which customers churned or downgraded, and the plan to stop it.",
  burn: "Ask what the extra spend buys and when it pays back.",
  team: "Ask who left, why, and what is at risk on the roadmap.",
  language: "Call the founder this week and ask about it directly.",
  silence: "Call the founder this week and ask for the numbers.",
};

const usd = (n) => (n >= 1e6 ? `$${(n / 1e6).toFixed(1)}M` : `$${Math.round(n / 1e3)}K`);

/** result: the watcher's { summary, sources, data }. */
export function renderDigest(result) {
  const { claim, table } = citations();
  const { asOf, counts, companies, report, registry } = result.data;
  const out = [
    "# Portfolio early warnings",
    "",
    `> Synthetic data. Built from founder updates up to ${asOf}. Every claim cites the exact file version it came from ([S1] etc., listed under Sources).`,
    "",
    `- ${claim(`${counts.red} red, ${counts.amber} amber and ${counts.green} green of ${companies.length} companies.`, [report])}`,
    "",
  ];
  for (const [level, heading] of [["red", "Red: act this week"], ["amber", "Amber: ask a question"]]) {
    const group = companies.filter((c) => c.level === level);
    out.push(`## ${heading}`, "");
    if (!group.length) out.push("None.", "");
    for (const c of group) {
      out.push(`### ${c.name}`, "");
      if (c.invested != null && registry) out.push(`- ${claim(`Position: ${usd(c.invested)} invested for ${c.ownership}% (${c.sector}).`, [registry])}`);
      for (const s of c.signals) out.push(`- ${claim(s.text, [c.source])} Next step: ${NEXT_STEP[s.rule]}`);
      out.push("");
    }
  }
  const green = companies.filter((c) => c.level === "green");
  out.push("## Green: no signals", "", `- ${claim(`${green.map((c) => c.name).join(", ")}.`, [report])}`, "", table(), "");
  return out.join("\n");
}
