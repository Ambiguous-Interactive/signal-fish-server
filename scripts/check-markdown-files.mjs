import { globSync } from "node:fs";
import { readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const [, , packageRoot, mode, ...paths] = process.argv;
const require = createRequire(join(packageRoot, "package.json"));
const load = (entry) => import(pathToFileURL(require.resolve(entry)).href);
const { lint, readConfig } = await load("markdownlint/promise");
const { applyFixes } = await load("markdownlint");

const files = paths[0] === "--discover"
  ? globSync("**/*.md", {
      exclude: ["target", "third_party", "**/node_modules", ".github/test-fixtures", "test-fixtures"],
    })
  : paths;
const options = { files, config: await readConfig(".markdownlint.json"), resultVersion: 3 };
let results = await lint(options);
if (mode === "fix") {
  for (const file of files) {
    const fixes = results[file];
    if (!fixes?.some(({ fixInfo }) => fixInfo)) {
      continue;
    }
    const original = await readFile(file, "utf8");
    const fixed = applyFixes(original, fixes);
    if (fixed !== original) {
      await writeFile(file, fixed, "utf8");
    }
  }
  results = await lint(options);
}

for (const [file, errors] of Object.entries(results)) {
  for (const error of errors) {
    const column = error.errorRange?.[0] ?? 1;
    const detail = [error.errorDetail, error.errorContext].filter(Boolean).join(" ");
    console.error(
      `${file}:${error.lineNumber}:${column} ${error.ruleNames.join("/")} ${error.ruleDescription} ${detail}`.trimEnd(),
    );
    process.exitCode = 1;
  }
}
