# Sourcing thesis (synthetic)

> Synthetic example for a fictional pre-seed and seed fund. Edit it to match your own. The scout bot's scoring in `bots/scout.mjs` follows this table: if you change one, change the other.

- **Data as of:** 2026-10-15
- **Stage:** pre-seed and seed
- **Focus:** developer tools, fintech infrastructure, climate software, applied health, robotics and security

## What we mean by exceptional

1. **They ship.** A product in real use beats an idea, a paper or a pitch.
2. **Independent signals converge.** Code, customers, competitions and a career move that all point at the same company, within six months, are rare.
3. **We are early.** A closed institutional round means others got there first. A Form D showing a round that is still open means the founder is meeting investors now.

## Signals and weights

| Signal | Counts when | Points |
|---|---|---|
| shipped | a release or launch with users | 3, plus 1 for 1,000+ weekly active users, plus 1 for paying customers |
| repo | active code, not a list, fork or tutorial | 2 for 150+ commits in 90 days (1 for 50+), plus 1 for 500+ stars |
| hackathon | 1st, 2nd or 3rd place | 2 for 1st, 1 otherwise, plus 1 when 100+ teams entered |
| paper | the founder is first author | 1 |
| job_change | left a job to work on the company full time | 2 |

Points are discounted by age: full within 90 days, half within a year, one fifth after that. Three kinds of signal in the last six months add 2; four or more add 3. A Form D with a round still open (sold less than offered, filed in the last 120 days) adds 1; $5M or more already sold subtracts 3.

## Not signals

- Stars on a list of links, a fork or a tutorial repo.
- Co-authored papers where the founder is not first author.
- Joining another company.
- Prizes from more than a year ago with nothing built since.
