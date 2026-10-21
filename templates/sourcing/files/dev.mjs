// Starts the scout bot. Stop with Ctrl+C.
import { Roundtable } from "@roundtable/sdk";
import * as scout from "./bots/scout.mjs";
import { identity, required } from "./lib/env.mjs";

const project = required("ROUNDTABLE_PROJECT");
const stop = new AbortController();
process.once("SIGINT", () => stop.abort());

const { name, key } = identity(scout.id);
console.log(`${name}: waiting for tasks in ${project}`);
await new Roundtable({ key })
  .bot({ name, project, skills: scout.skills })
  .onTask(scout.handle)
  .run({ signal: stop.signal });
