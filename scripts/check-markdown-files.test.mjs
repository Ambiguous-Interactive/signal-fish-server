import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";

const root = dirname(import.meta.dirname);

function fixture(t) {
  const cwd = mkdtempSync(join(tmpdir(), "markdown-runner-"));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  copyFileSync(join(root, ".markdownlint.json"), join(cwd, ".markdownlint.json"));
  return cwd;
}

function run(cwd, mode, ...paths) {
  const result = spawnSync(
    process.execPath,
    [
      join(root, "scripts/check-markdown-files.mjs"),
      join(root, "node_modules/markdownlint"),
      mode,
      ...paths,
    ],
    { cwd, encoding: "utf8" },
  );
  assert.ifError(result.error);
  return result;
}

test("reports rule and location for a named file with spaces", (t) => {
  const cwd = fixture(t);
  const path = "with spaces.md";
  writeFileSync(join(cwd, path), `# Title\n\n${"words ".repeat(25)}end\n`);
  const result = run(cwd, "check", path);
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /with spaces\.md:3:121 MD013\/line-length/);
});

test("fixes a document and rechecks the result", (t) => {
  const cwd = fixture(t);
  const path = "fix.md";
  writeFileSync(join(cwd, path), "# Title\n\nText   \n");
  const result = run(cwd, "fix", path);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(join(cwd, path), "utf8"), "# Title\n\nText\n");
});

test("discovers archive Markdown and skips dependencies and test fixtures", (t) => {
  const cwd = fixture(t);
  for (const path of [
    "README.md",
    "node_modules/package/README.md",
    "bootstrap/node_modules/package/README.md",
    "third_party/README.md",
    ".github/test-fixtures/README.md",
  ]) {
    mkdirSync(dirname(join(cwd, path)), { recursive: true });
    writeFileSync(join(cwd, path), `# Title\n\n${"words ".repeat(25)}end\n`);
  }
  const result = run(cwd, "check", "--discover");
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /^README\.md:3:/);
  assert.doesNotMatch(result.stderr, /node_modules|third_party|test-fixtures/);
});
