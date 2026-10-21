// Starts every bot in this folder. Stop with Ctrl+C.
import { Roundtable } from "@roundtable/sdk";
import * as redFlags from "./bots/red-flags.mjs";
import * as comps from "./bots/comps.mjs";
import * as market from "./bots/market.mjs";
import { identity, required } from "./lib/env.mjs";

const project = required("ROUNDTABLE_PROJECT");
const stop = new AbortController();
process.once("SIGINT", () => stop.abort());

await Promise.all(
  [redFlags, comps, market].map((bot) => {
    const { name, key } = identity(bot.id);
    console.log(`${name}: waiting for tasks in ${project}`);
    return new Roundtable({ key })
      .bot({ name, project, skills: bot.skills })
      .onTask(bot.handle)
      .run({ signal: stop.signal });
  }),
);
