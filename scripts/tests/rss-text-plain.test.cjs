const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const source = fs.readFileSync(
  path.join(__dirname, "../../src/utils/string.ts"),
  "utf8"
);
const cleanHtmlText = source.slice(
  source.indexOf("export const cleanHtmlText"),
  source.indexOf("export const removeFileExt")
);

test("RSS summaries use an inert DOMParser document, never live innerHTML", () => {
  assert.match(cleanHtmlText, /new DOMParser\(\)/);
  assert.match(
    cleanHtmlText,
    /parseFromString\(unicodeDecoded, "text\/html"\)/
  );
  assert.match(cleanHtmlText, /parsed\.body\.textContent/);
  assert.doesNotMatch(cleanHtmlText, /innerHTML|document\.createElement/);
});

test("legacy Unicode escapes are still decoded before HTML text extraction", () => {
  assert.match(cleanHtmlText, /input\.replace\(\/\\\\u\(/);
  assert.ok(
    cleanHtmlText.indexOf("const unicodeDecoded") <
      cleanHtmlText.indexOf("parseFromString")
  );
});
