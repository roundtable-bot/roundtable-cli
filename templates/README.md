# Templates

Starting points for the hackathon tracks. `roundtable init <dir> --template <track>` loads a template's `seed/` into your bank and copies its `files/` into a new project folder.

| Track | Template | Bots | Synthetic data |
|---|---|---|---|
| Diligence | [`diligence/`](diligence) | `red-flags`, `comps`, `market`, `memo-writer` | A data room for "Acme Ledger, Inc." with six planted red flags |
| Sourcing | [`sourcing/`](sourcing) | `scout`, `shortlist` | 40 founder profiles with public signals and 16 Form D-style filings |
| Portfolio | [`portfolio/`](portfolio) | `watcher`, `digest` | 30 companies, six monthly founder updates each |
| Funds | [`funds/`](funds) | `fund-reader`, `quarter-brief` | Two funds over two quarters: reports, K-1 summaries, capital calls and distributions |

Each template has the same layout:

```
<track>/
  template.json   manifest: bots (name, role, env), seed (from -> to bank path), next steps
  seed/           synthetic data, written into the bank by init
  files/          the project folder: package.json, dev.mjs, bots/, an orchestrator, README.md, answer-key.md
  tests/          not copied: unit tests that run the bots against seed/
```

All companies, people and numbers are fictional. `answer-key.md` lists what each starter bot should find; it lives in `files/`, never in `seed/`, so the bots cannot read it.

## Checks

From the repository root, with Node.js 20.6 or newer and no installs:

```sh
node --test templates/*.test.mjs templates/*/tests/*.test.mjs
```

`templates/check.test.mjs` validates every `template.json` against the manifest contract (roles, `<NAME>_KEY` env names, every seed `from` exists, every `to` is a valid bank path, files within the 200,000-character and 2,000-file bank limits) and runs `node --check` on every `.mjs`. The per-template tests run the starter bots against the seed data: the planted problems must be found, and a clean control case must produce none where one exists.

## Adding a template

Add `<track>/` with the layout above, give every bot an `id` that matches its manifest name, read keys, names and ids through `lib/env.mjs` (`<NAME>_KEY`, `<NAME>_NAME` and `<NAME>_ID`; dispatch with `botId()`), keep rule logic in pure functions so the tests can run it without the SDK, and make sure every claim in the orchestrator's output carries a citation (bank, path, version). Never put credentials in a template.
