# Answer key: planted red flags in the Acme Ledger data room

Spoilers. Look at this only after you have run your own checks, or to see what `bots/red-flags.mjs` is expected to find. This file lives in your project folder, not in the data room, so the bots never read it. The data room is synthetic.

All figures are as of 30 September 2026. The flag ids below are the ids returned by `bots/red-flags.mjs`.

| # | Id | Severity | Where to look |
|---|---|---|---|
| 1 | `arr-includes-one-time` | high | `deck.md` traction table, `financial-model.csv` |
| 2 | `cohort-churn-hidden` | high | `deck.md` retention slide, `cohorts.csv` |
| 3 | `burn-vs-hiring-plan` | high | `financial-model.csv` forecast, `hiring-plan.csv` |
| 4 | `deck-model-mismatch` | medium | `deck.md` traction table, `financial-model.csv`, `cohorts.csv` |
| 5 | `customer-concentration` | high | `deck.md` customers slide, `contracts-summary.md` |
| 6 | `renewal-cliff` | medium | `contracts-summary.md` |

## 1. `arr-includes-one-time`: non-recurring revenue counted as ARR

- The deck says ARR is **$4.8M** and footnotes it as "September 2026 monthly revenue annualised".
- September revenue in the model is $312,000 subscription **plus $88,000 implementation fees** = $400,000. $400,000 x 12 = $4.8M.
- Implementation fees are one-time (the contract summary says so). Recurring ARR is $312,000 x 12 = **$3.744M**, which is $1.056M (22%) lower.

## 2. `cohort-churn-hidden`: churn hidden in the cohorts

- The deck claims **121% net revenue retention** and **7% annual logo churn**.
- In `cohorts.csv`, only **61%** of logos are still active after 12 months across the 11 cohorts old enough to measure, which is **39% annual logo churn**.
- The 121% is real, but one account is carrying it: the 2024Q1 cohort retains 266% of its revenue because of a single customer that grew from 3 to 41 business units (Halden Freight Group). Without that cohort, dollar retention is 94%.
- 12-month logo retention is also slipping: about 65% for the older cohorts and 59% for the newer ones.

## 3. `burn-vs-hiring-plan`: burn does not match the hiring plan

- `hiring-plan.csv` has **17 hires** starting October 2026 to June 2027, costing about **$2.45M** loaded over the 12 forecast months.
- The model's forecast payroll rises by only about **$341K** over the same months, and headcount goes from 38 to 40.
- Adding the missing payroll, cash lasts about **10 months** from September 2026 (to roughly June 2027). The model implies 15.5 months and the deck says 15.

## 4. `deck-model-mismatch`: numbers differ between deck and model

- **Gross margin:** the deck says **82%**. The model gives about **73%** for Q3 2026 (cost of revenue over total revenue).
- **Customers:** the deck says **148**. Summing the latest active logos per cohort in `cohorts.csv` gives **121**, and the contract summary also says 121 active customers.
- Net burn ($310K), cash ($4.7M) and Q3 revenue ($1.13M) do agree; they are there so that a rule has something to get right.

## 5. `customer-concentration`: one customer is about a third of ARR

- The deck says "No customer is more than 10% of ARR".
- `contracts-summary.md`: Halden Freight Group pays **$1.16M** a year, **31%** of recurring ARR ($3.744M). It is 24% even of the inflated $4.8M.

## 6. `renewal-cliff`: contracts that can end soon

- Halden Freight Group ends **31 January 2027**, does not auto-renew, can terminate for convenience at 60 days' notice, and asked for a 20% price cut on 18 September 2026.
- Corvane Wholesale (9% of ARR) ends **31 December 2026** with no auto-renewal and has not answered the renewal proposal.
- Together that is **40%** of recurring ARR up for renewal within about four months.

## Decoys (not flags)

- Ostrander Hardware ends 28 February 2027 but auto-renews, so the rule must not count it.
- Net revenue retention, net burn, cash and Q3 revenue in the deck match the files.
