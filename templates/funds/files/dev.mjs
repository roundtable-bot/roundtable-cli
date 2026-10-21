// Starts the fund-reader bot. Stop with Ctrl+C.
import { Roundtable } from "@roundtable/sdk";
import * as fundReader from "./bots/fund-reader.mjs";
import { identity, required } from "./lib/env.mjs";

const project = required("ROUNDTABLE_PROJECT");
const stop = new AbortController();
process.once("SIGINT", () => stop.abort());

const { name, key } = identity(fundReader.id);
console.log(`${name}: waiting for tasks in ${project}`);
await new Roundtable({ key })
  .bot({ name, project, skills: fundReader.skills })
  .onTask(fundReader.handle)
  .run({ signal: stop.signal });
