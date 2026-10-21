# Answer key: what the fund-reader should find

Spoilers. Look at this after you have run the brief, or to check a change to the checks. This file lives in your project folder, not in the bank. Both funds, the investor and every number are synthetic.

You are Birchwood Family Office, a limited partner in two funds. There are two quarters of reports (Q1 and Q2 2026), the capital call and distribution notices for them, and a K-1 summary for tax year 2025 for each fund.

## Three things that do not add up

The bot runs 20 cross-checks per fund. Exactly these three fail:

| Fund | Quarter | Check id | What is wrong |
|---|---|---|---|
| Halyard Ventures Fund III | Q2 2026 | `calls-vs-notices` | The report shows **$12,600,000** called ($420,000 from you). The two call notices in the quarter (14 April, $7.2M, and 3 June, $4.9M) total **$12,100,000** ($403,333 from you). $500,000 at fund level, $16,667 of your money, is not explained by any notice. |
| Cinder Peak Growth Partners II | Q2 2026 | `fund-rollforward` | Opening NAV $293,930K plus the quarter's flows is **$289,745K**, but the report states closing NAV of **$287,905K**: $1,840K (0.6%) unexplained. |
| Cinder Peak Growth Partners II | Q2 2026 | `lp-vs-fund` | Your ending capital account ($2,759,477) follows the roll-forward, not the reported NAV: the reported fund NAV times your 0.95% share is $2,741,952, $17,525 lower. Same root cause as the line above. |

Everything else ties: continuity from Q1 to Q2, paid-in capital against the K-1, unfunded commitments, DPI and TVPI, the valuation tables against the unrealized-appreciation line, and the K-1 capital account analysis.

## What moved from Q1 to Q2 (your capital account)

**Halyard Ventures Fund III** (early-stage venture): $3,594,000 to $4,093,500, up $499,500. You funded $420,000 of calls and the fund paid $36,500 of fees and expenses; unrealized value rose $116,000. Under the surface, Kestrelon Labs fell **$9.8M** at fund level (a down round priced 40% below its Series B), offset by Tessellate AI **+$6.7M** (Series B at 1.8x), Cobblestone Energy +$2.7M and Halcyon Security +$1.6M. No distributions.

**Cinder Peak Growth Partners II** (growth equity): $2,799,334 to $2,759,477, down $39,857, even though TVPI rose from 1.45x to 1.49x. You received **$200,000** (a secondary sale of one third of Ardent Freight, $11.4M realized gain at fund level) and paid in $20,000. Skyharbor Data fell **$4.5M** (missed plan), Hollis Payments rose $3.5M on an unclosed acquisition offer.
