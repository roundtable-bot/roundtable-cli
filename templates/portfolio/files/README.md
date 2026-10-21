# Portfolio: early warnings from founder updates

The `watcher` bot reads six monthly updates (April to September 2026) from each of 30 synthetic portfolio companies, pulls runway, burn, revenue and headcount out of the free text, and raises early warnings a fund can act on. `digest.mjs` asks the watcher to review the whole portfolio and saves a digest in which every claim cites the exact file version it came from, with a suggested next step for each warning.

| Piece | File | What it does |
|---|---|---|
| `watcher` | `bots/watcher.mjs` | Extracts numbers from each update and applies the early-warning rules. **This is the one you edit.** |
| `digest` | `digest.mjs` | Dispatches to the watcher, gathers the findings, writes `early-warnings.md`. |

The data is in your bank under `portfolio/`: `companies.csv`, `watch-policy.md` and `updates/` (one file per company, six updates each, in the formats founders really use: structured lists, prose and terse one-liners). `answer-key.md` says which companies the watcher should flag; read it last.

## Run it

```sh
npm install
npm run dev      # starts the watcher; leave it running
npm run digest   # in a second terminal: saves early-warnings.md and portfolio/early-warnings.md in the bank
```

If `npm run digest` times out after five minutes, the watcher is not running: check `npm run dev`.

`@roundtable/sdk` is not published to npm yet. If `npm install` fails with a 404, see `docs/sdk-quickstart.md` in the Roundtable repository for how to install the SDK from a local checkout.

## Make it yours

1. **Change a rule.** `assess()` in `bots/watcher.mjs` holds the rules, and `portfolio/watch-policy.md` in the bank describes them, so change both. For example, make runway red below 9 months, or add a rule for customer concentration.
2. **Read more formats.** `extractMetrics()` uses a few patterns for MRR, cash, burn, runway and headcount. An update it cannot read gets null metrics, and the "going dark" rule notices. Add a pattern, or use the LLM hook.
3. **Add an LLM.** `askModel()` in `bots/watcher.mjs` is marked. It receives an update the patterns could not read and returns its numbers.
4. **Use your own companies.** Put your updates in the bank as `portfolio/updates/<company>.md` (same layout: `Date:` and `Subject:` headers, updates separated by `---`) and list the companies in `portfolio/companies.csv`.

After editing, stop `npm run dev` (Ctrl+C), start it again and rerun `npm run digest`.

## Settings

`roundtable init` wrote `.env` with `ROUNDTABLE_URL`, `ROUNDTABLE_PROJECT` (the bank), and a key and name for each bot (`WATCHER_KEY`, `WATCHER_NAME`, `DIGEST_KEY`, `DIGEST_NAME`). The code reads names from `*_NAME`. `.env` is git-ignored: never commit it.

All companies, founders and numbers are fictional.
