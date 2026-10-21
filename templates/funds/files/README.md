# Funds: what moved since last quarter

You are a limited partner in two synthetic funds. The `fund-reader` bot reads a fund's quarterly reports, capital call and distribution notices and K-1 summary, pulls out NAV, capital calls, distributions and valuation changes, explains what moved since last quarter, and cross-checks the documents against each other. `quarter-brief.mjs` asks it about both funds and saves a brief in which every claim cites the exact file version it came from.

| Piece | File | What it does |
|---|---|---|
| `fund-reader` | `bots/fund-reader.mjs` | Parses the documents, builds the NAV bridge and runs 20 cross-checks per fund. **This is the one you edit.** |
| `quarter-brief` | `quarter-brief.mjs` | Reads `funds/lp.md`, dispatches one task per fund, gathers the results, writes `quarter-brief.md`. |

The data is in your bank under `funds/`: `lp.md` (your commitments), then a folder per fund with `q1-2026-report.md`, `q2-2026-report.md`, `capital-call-*.md`, `distribution-notice-*.md` and `k1-summary-2025.md`. `answer-key.md` says what the bot should find; read it last.

## Run it

```sh
npm install
npm run dev     # starts the fund-reader; leave it running
npm run brief   # in a second terminal: saves quarter-brief.md and funds/briefs/latest.md in the bank
```

If `npm run brief` times out after five minutes, the fund-reader is not running: check `npm run dev`.

## Make it yours

1. **Add a check.** `checkFund()` in `bots/fund-reader.mjs` builds the list of checks with `add(id, quarter, label, ok, detail)`. For example, compare management fees with 2% of the commitment, or flag a single markdown larger than 10% of NAV.
2. **Read a different report layout.** `ROWS` at the top of the file maps row labels to fields. A report that words a row differently is skipped, not misread: add a pattern.
3. **Add an LLM.** `askModel()` in `bots/fund-reader.mjs` is marked. The manager commentary in each report is free text the bot does not read; summarise it there.
4. **Use real reports.** Put your own in the bank as `funds/<fund>/q3-2026-report.md` and so on, and list the fund in `funds/lp.md`. Real reports vary a lot: expect to adjust `ROWS`.

After editing, stop `npm run dev` (Ctrl+C), start it again and rerun `npm run brief`.

## Settings

`roundtable init` wrote `.env` with `ROUNDTABLE_URL`, `ROUNDTABLE_PROJECT` (the bank), and a key and name for each bot (`FUND_READER_KEY`, `FUND_READER_NAME`, `QUARTER_BRIEF_KEY`, `QUARTER_BRIEF_NAME`). The code reads names from `*_NAME`. `.env` is git-ignored: never commit it.

All funds, companies, investors and numbers are fictional.
