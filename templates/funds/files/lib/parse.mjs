// Small parsers shared by the bot and the brief. No dependencies.

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

/** "(1,260)" -> -1260, "$7,200,000" -> 7200000, "1.24x" -> 1.24, "" -> null. */
export function parseAmount(text) {
  const m = /^\s*(\()?\s*-?\$?\s*(\d[\d,]*(?:\.\d+)?)\s*([kmb])?x?\s*\)?\s*$/i.exec(String(text));
  if (!m) return null;
  const scale = { k: 1e3, m: 1e6, b: 1e9 }[(m[3] ?? "").toLowerCase()] ?? 1;
  const value = Number(m[2].replaceAll(",", "")) * scale;
  return value && (m[1] || String(text).trim().startsWith("-")) ? -value : value;
}

/** Splits a document into { title -> text } by its "## " headings. */
export function sections(markdown) {
  const out = {};
  for (const part of markdown.split(/^## /m).slice(1)) {
    const [title, ...rest] = part.split("\n");
    out[title.trim()] = rest.join("\n");
  }
  return out;
}

/** The value of a "- **Label:** value" line, or null. */
export function field(markdown, label) {
  return new RegExp(`^- \\*\\*${label}:\\*\\* (.+)$`, "m").exec(markdown)?.[1].trim() ?? null;
}
