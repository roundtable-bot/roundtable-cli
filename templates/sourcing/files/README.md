# Sourcing: find exceptional founders before the round

The `scout` bot reads 40 synthetic founder profiles (public signals: shipped projects, GitHub-style activity, papers, hackathon wins, job changes) and 16 Form D-style filings from your bank, ranks the founders, and explains why each of the top few deserves a meeting. `shortlist.mjs` asks the scout for the top 8 and saves a shortlist in which every claim cites the exact file version it came from.

| Piece | File | What it does |
|---|---|---|
| `scout` | `bots/scout.mjs` | Scores every profile from its signals and any Form D that lists the founder. **This is the one you edit.** |
| `shortlist` | `shortlist.mjs` | Dispatches to the scout, gathers the picks, writes `shortlist.md`. |

The data is in your bank under `sourcing/`: `thesis.md`, `profiles/` and `filings/`. `answer-key.md` says who the scout should find; read it last.

## Run it

```sh
npm install
npm run dev         # starts the scout; leave it running
npm run shortlist   # in a second terminal: saves shortlist.md and sourcing/shortlist.md in the bank
```

If `npm run shortlist` times out after five minutes, the scout is not running: check `npm run dev`.

`@roundtable/sdk` is not published to npm yet. If `npm install` fails with a 404, see `docs/sdk-quickstart.md` in the Roundtable repository for how to install the SDK from a local checkout.

## Make it yours

1. **Change the scoring.** `scoreFounder()` and `weigh()` in `bots/scout.mjs` turn signals into points. They follow the table in `sourcing/thesis.md` in the bank, so change both. For example, reward `paper` signals more, or count a hackathon only when it was won.
2. **Add a signal type.** Add a `case` in `weigh()` and rows of that type to a profile.
3. **Add an LLM.** `askModel()` in `bots/scout.mjs` is marked. It gets each pick and can add a sentence of judgement to its "why".
4. **Use real data.** Replace the seed profiles with your own in the bank (same table format), or write a bot that fills `sourcing/profiles/` from the sources you have access to. Check each source's terms first.

After editing, stop `npm run dev` (Ctrl+C), start it again and rerun `npm run shortlist`.

## Settings

`roundtable init` wrote `.env` with `ROUNDTABLE_URL`, `ROUNDTABLE_PROJECT` (the bank), and a key and name for each bot (`SCOUT_KEY`, `SCOUT_NAME`, `SHORTLIST_KEY`, `SHORTLIST_NAME`). The code reads names from `*_NAME`. `.env` is git-ignored: never commit it.

All founders, companies and filings are fictional.
