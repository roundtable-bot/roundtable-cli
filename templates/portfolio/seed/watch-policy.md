# Early-warning policy (synthetic)

> Synthetic example for a fictional fund. Edit it to match your own. The watcher bot's rules in `bots/watcher.mjs` follow this table: if you change one, change the other.

Each company sends a monthly update. The watcher reads the latest six and raises a signal when:

| Signal | Level | Rule |
|---|---|---|
| Runway | red below 6 months, amber below 9 | Cash divided by net burn from the latest update that reports both. |
| Revenue falling | red | MRR fell three months in a row, or is 20% or more below its peak in the last six months. |
| Revenue falling | amber | MRR fell two months in a row. |
| Burn creeping up | amber | Net burn is 30% or more higher than four months earlier. |
| Team shrinking | amber | Headcount is 15% or more below its peak. |
| Risk language | amber | The last three updates mention a resignation, layoff, pivot, delay or a customer that did not renew. |
| Going dark | amber | The last two updates report no cash, burn or runway. |

A company is **red** if any signal is red, or if two or more different signals are amber. It is **amber** with one amber signal, and **green** with none.

What we do about each: ask for a cash plan and the status of any raise (runway); ask which customers churned and why (revenue); ask what the extra spend buys (burn); call the founder this week (going dark, resignations).
