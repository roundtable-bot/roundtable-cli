// Settings come from .env, which `roundtable init` wrote (npm scripts load it with --env-file).

export function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set. Run \`roundtable init\`, or add it to .env.`);
  return value;
}

const prefix = (manifestName) => manifestName.toUpperCase().replaceAll("-", "_");

// init may add a suffix to a bot's name (red-flags-2) and records it as RED_FLAGS_NAME.
export const nameOf = (manifestName) => process.env[`${prefix(manifestName)}_NAME`] || manifestName;

// init also records each bot's agent id (RED_FLAGS_ID). Dispatch by id: names are only unique
// per team agent, so another team's "red-flags" in the same account could otherwise match.
export const botId = (manifestName) => process.env[`${prefix(manifestName)}_ID`] || nameOf(manifestName);

// A bot called "red-flags" has its key in RED_FLAGS_KEY.
export const identity = (manifestName) => ({ name: nameOf(manifestName), key: required(`${prefix(manifestName)}_KEY`) });
