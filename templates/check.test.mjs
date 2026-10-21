// Validates every template against the manifest contract and the bank limits.
// Run: node --test templates/*.test.mjs templates/*/tests/*.test.mjs
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { spawnSync } from "node:child_process";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const TRACKS = ["diligence", "sourcing", "portfolio", "funds"];
const ROLES = ["researcher", "reviewer", "writer", "coordinator"];
const MAX_FILE_CHARS = 200_000; // bank limit per file
const MAX_FILES = 2_000; // bank limit per bank

const walk = (dir) =>
  readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? (name === "node_modules" ? [] : walk(path)) : [path];
  });

/** The server's path rule: relative POSIX, segments [A-Za-z0-9._-]+, not . or .., at most 8 segments and 300 chars. */
function bankPathProblem(path) {
  if (typeof path !== "string" || !path) return "empty";
  if (path.length > 300) return "longer than 300 characters";
  const segments = path.split("/");
  if (segments.length > 8) return "more than 8 segments";
  const bad = segments.find((s) => !/^[A-Za-z0-9._-]+$/.test(s) || s === "." || s === "..");
  return bad === undefined ? null : `bad segment "${bad}"`;
}

const templates = TRACKS.map((track) => ({ track, dir: join(root, track) }));

test("the four tracks exist", () => {
  assert.deepEqual(readdirSync(root).filter((n) => statSync(join(root, n)).isDirectory() && TRACKS.includes(n)).sort(), [...TRACKS].sort());
});

for (const { track, dir } of templates) {
  test(`${track}: template.json follows the manifest contract`, () => {
    const manifest = JSON.parse(readFileSync(join(dir, "template.json"), "utf8"));
    assert.deepEqual(Object.keys(manifest).sort(), ["bots", "next_steps", "seed", "title", "track"]);
    assert.equal(manifest.track, track);
    assert.ok(typeof manifest.title === "string" && manifest.title.length > 0);

    assert.ok(manifest.bots.length >= 2, "needs at least a worker and an orchestrator");
    const names = new Set();
    const envs = new Set();
    for (const bot of manifest.bots) {
      assert.deepEqual(Object.keys(bot).sort(), ["env", "name", "role"]);
      assert.match(bot.name, /^[a-z][a-z0-9-]*$/, `bot name ${bot.name}`);
      assert.ok(ROLES.includes(bot.role), `${bot.name}: role ${bot.role}`);
      assert.equal(bot.env, `${bot.name.toUpperCase().replaceAll("-", "_")}_KEY`, `${bot.name}: env name`);
      assert.ok(!names.has(bot.name) && !envs.has(bot.env), `${bot.name} is duplicated`);
      names.add(bot.name);
      envs.add(bot.env);
    }

    assert.ok(manifest.next_steps.length > 0 && manifest.next_steps.every((s) => typeof s === "string"));
    const pkg = JSON.parse(readFileSync(join(dir, "files/package.json"), "utf8"));
    for (const step of manifest.next_steps) {
      const script = /^npm run (\S+)$/.exec(step)?.[1];
      if (script) assert.ok(pkg.scripts[script], `next step "${step}" has no script in package.json`);
      else assert.equal(step, "npm install");
    }
  });

  test(`${track}: seed entries exist, land on valid bank paths and fit the limits`, () => {
    const manifest = JSON.parse(readFileSync(join(dir, "template.json"), "utf8"));
    assert.ok(manifest.seed.length > 0 && manifest.seed.length < MAX_FILES, "seed count");
    const targets = new Set();
    for (const { from, to, ...extra } of manifest.seed) {
      assert.deepEqual(extra, {}, `${from}: unexpected keys`);
      assert.ok(from.startsWith("seed/") && !from.split("/").includes(".."), `${from} must be inside seed/`);
      assert.equal(bankPathProblem(to), null, `${to}: ${bankPathProblem(to)}`);
      assert.ok(!targets.has(to), `${to} is seeded twice`);
      targets.add(to);

      const text = readFileSync(join(dir, from), "utf8"); // throws if missing
      assert.ok(!text.includes("\uFFFD"), `${from} is not valid UTF-8`);
      assert.ok(text.length <= MAX_FILE_CHARS, `${from} has ${text.length} characters`);
      if (!from.endsWith(".csv")) assert.match(text, /synthetic|fictional/i, `${from} must say it is synthetic`);
    }

    const onDisk = walk(join(dir, "seed")).map((p) => relative(dir, p).split("\\").join("/"));
    assert.deepEqual(onDisk.sort(), manifest.seed.map((s) => s.from).sort(), "every file in seed/ is in the manifest, and vice versa");
    assert.ok(!onDisk.some((p) => /answer-key/i.test(p)), "the answer key must not be seeded");
  });

  test(`${track}: files/ is a complete, runnable project`, () => {
    const files = walk(join(dir, "files"));
    assert.ok(files.length < MAX_FILES);
    const pkg = JSON.parse(readFileSync(join(dir, "files/package.json"), "utf8"));
    assert.equal(pkg.dependencies["@roundtable/sdk"], "^0.1.0");
    assert.equal(Object.keys(pkg.dependencies).length, 1, "no dependencies besides the SDK");
    assert.ok(pkg.scripts.dev, "dev script");
    assert.equal(pkg.type, "module");
    for (const required of ["README.md", "answer-key.md", ".gitignore", "dev.mjs", "lib/env.mjs"]) {
      assert.ok(files.some((f) => relative(join(dir, "files"), f) === required), `files/${required}`);
    }
    assert.ok(!files.some((f) => /(^|\/)\.env(\.|$)/.test(f)), "no .env in files/");

    const code = files.filter((f) => f.endsWith(".mjs")).map((f) => readFileSync(f, "utf8")).join("\n");
    const manifest = JSON.parse(readFileSync(join(dir, "template.json"), "utf8"));
    for (const bot of manifest.bots) assert.ok(code.includes(`"${bot.name}"`), `no code mentions bot "${bot.name}"`);
    assert.ok(/ROUNDTABLE_PROJECT/.test(code), "reads the bank from ROUNDTABLE_PROJECT");
    assert.ok(!/Bearer [A-Za-z0-9]{12,}|sk-[A-Za-z0-9]{12,}/.test(code), "no credentials in code");
  });

  test(`${track}: every .mjs passes node --check`, () => {
    for (const file of walk(dir).filter((f) => f.endsWith(".mjs"))) {
      const run = spawnSync(process.execPath, ["--check", file], { encoding: "utf8" });
      assert.equal(run.status, 0, `${relative(root, file)}: ${run.stderr}`);
    }
  });
}
