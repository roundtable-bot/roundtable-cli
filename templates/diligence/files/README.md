# Diligence: underwrite a startup in an hour

Three bots work on a synthetic data room for **Acme Ledger, Inc.** (fictional) and a fourth, `memo-writer`, combines their results into a first-draft memo in which every claim cites the exact file version it came from.

| Bot | File | What it does |
|---|---|---|
| `red-flags` | `bots/red-flags.mjs` | Recomputes ARR, churn, burn and runway from the files and compares them with the deck. **This is the one you edit.** |
| `comps` | `bots/comps.mjs` | Public comparables from SEC EDGAR. Falls back to labelled sample data. |
| `market` | `bots/market.mjs` | Market signals. Akta adapter plus labelled synthetic data. |
| `memo-writer` | `memo-writer.mjs` | Dispatches to the three bots, gathers their results, writes the memo. |

The data room is in your bank under `dataroom/acme/`: a deck, a financial model, cohorts, a hiring plan and a contract summary. It contains deliberate problems for the bots to find. `answer-key.md` lists them, so read it last.

## Run it

```sh
npm install
npm run dev     # starts the three bots; leave it running
npm run memo    # in a second terminal: sends work to the bots and saves memos/acme.md
```

`npm run memo` also saves the memo to the bank as `memos/acme.md`. If it times out after five minutes, the bots are not running: check that `npm run dev` is up.

`@roundtable/sdk` is not published to npm yet. If `npm install` fails with a 404, see `docs/sdk-quickstart.md` in the Roundtable repository for how to install the SDK from a local checkout.

## Make it yours

1. **Add a rule.** In `bots/red-flags.mjs`, write an object like the ones in `RULES` and add it to the list. A rule gets the data room as `{ "deck.md": "...", ... }` and returns `{ flag, metrics }`. Rules are pure functions, so you can try them in `node` without Roundtable.
2. **Add an LLM.** `askModel()` in `bots/red-flags.mjs` is marked. Send the room to your model there and return extra flags in the same shape. Keep `evidence.file` set to a file name: it becomes the citation.
3. **Use real market data.** `bots/market.mjs` reads `AKTA_API_URL` and `AKTA_API_KEY` from `.env`. The Akta call itself is a marked TODO: confirm the API with Wokelo, then fill in `fetchAktaSignals()`.
4. **Use real comps.** Add `SEC_USER_AGENT="Your Name you@example.com"` to `.env` (SEC requires a contact in the User-Agent). Without it, `comps` returns sample data and says so.
5. **Change the memo.** `lib/memo.mjs` builds it. `claim(text, sources)` refuses to print a claim without a source; keep it that way.

After editing a bot, stop `npm run dev` (Ctrl+C), start it again, and rerun `npm run memo`.

## Settings

`roundtable init` wrote `.env` with `ROUNDTABLE_URL`, `ROUNDTABLE_PROJECT` (the bank), and a key and name for each bot (for example `RED_FLAGS_KEY` and `RED_FLAGS_NAME`). The code reads names from `*_NAME`, so it still works if init had to add a suffix like `red-flags-2`. `.env` is git-ignored: never commit it.

All companies, people and numbers in the data room are fictional.
