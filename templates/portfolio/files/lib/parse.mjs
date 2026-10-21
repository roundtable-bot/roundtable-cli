// Small parser shared by the bot. No dependencies.

/** CSV text -> array of objects keyed by the header row. Handles "quoted, fields". */
export function parseCsv(text) {
  const [header, ...lines] = text.trim().split(/\r?\n/).map((line) => {
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
  });
  return lines.map((cells) => Object.fromEntries(header.map((h, i) => [h.trim(), (cells[i] ?? "").trim()])));
}
