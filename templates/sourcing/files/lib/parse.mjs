// Small parsers shared by the bot and its tests. No dependencies.

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

/** The value of a "- **Label:** value" line, or null. */
export function field(markdown, label) {
  return new RegExp(`^- \\*\\*${label}:\\*\\* (.+)$`, "m").exec(markdown)?.[1].trim() ?? null;
}

const SCALE = { k: 1e3, m: 1e6, b: 1e9 };

/** "$2.5M" -> 2500000, "1,240" -> 1240, "n/a" -> null. */
export function parseNumber(text) {
  const m = /(\d[\d,]*(?:\.\d+)?)\s*([kmb])?\b/i.exec(String(text));
  return m ? Number(m[1].replaceAll(",", "")) * (SCALE[(m[2] ?? "").toLowerCase()] ?? 1) : null;
}
