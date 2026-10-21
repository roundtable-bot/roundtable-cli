// Citations for the report: [S1], [S2], ... in the text, and a Sources table at the end that
// maps each one to the exact bank, path and file version a bot read.
export function citations() {
  const sources = [];
  const cite = (from) =>
    from
      .map((s) => {
        let i = sources.findIndex((x) => x.bank === s.bank && x.path === s.path && x.version === s.version);
        if (i < 0) i = sources.push(s) - 1;
        return `[S${i + 1}]`;
      })
      .join("");
  return {
    cite,
    // Every claim goes through here. A claim without a source is a bug, so it throws.
    claim(text, from) {
      if (!from?.length) throw new Error(`Claim without a source: ${text}`);
      return `${text} ${cite(from)}`;
    },
    table() {
      return ["## Sources", "", "| Ref | Bank | Path | Version |", "|---|---|---|---|", ...sources.map((s, i) => `| S${i + 1} | ${s.bank} | ${s.path} | ${s.version} |`)].join("\n");
    },
  };
}
