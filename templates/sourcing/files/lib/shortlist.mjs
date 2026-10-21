// Turns the scout's picks into a shortlist where every claim cites its source.
import { citations } from "./cite.mjs";

/** result: the scout's { summary, sources, data }. */
export function renderShortlist(result) {
  const { claim, table } = citations();
  const { today, scanned, picks, report } = result.data;
  const out = [
    "# Founder shortlist",
    "",
    `> Synthetic data. Ranked by the scout bot from public signals as of ${today}. Every claim cites the exact file version it came from ([S1] etc., listed under Sources).`,
    "",
    claim(`Scanned ${scanned} founder profiles; the ${picks.length} below score highest.`, [report]),
    "",
  ];
  picks.forEach((p, i) => {
    const [profile, filing] = p.sources;
    out.push(`## ${i + 1}. ${p.name}: ${p.company} (score ${p.score})`, "", `**Why meet:** ${claim(p.why, p.sources)}`, "");
    for (const e of p.evidence) out.push(`- ${e.date} (${e.type}): ${claim(e.text, [profile])}`);
    out.push(`- Timing: ${claim(p.timing, [filing ?? profile])}`, "");
  });
  out.push(table(), "");
  return out.join("\n");
}
