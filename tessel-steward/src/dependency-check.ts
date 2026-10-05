/** Exit code of the dependency check when `package.json` declares any dependency. */
export const DEPENDENCIES_DECLARED_EXIT_CODE = 3;
/** Exit code of the dependency check when there is no `package.json`. */
export const PACKAGE_JSON_MISSING_EXIT_CODE = 4;
/** Exit code of the dependency check when `package.json` is not a JSON object. */
export const PACKAGE_JSON_INVALID_EXIT_CODE = 5;

/**
 * Script for `node -e`, run in the repo root: exits 0 when `package.json` declares no
 * dependencies of any kind, 3 when it declares any, 4 when it is missing and 5 when it is not
 * a JSON object. The built-in module is not `require`d: from Node 22.23.3 on, `require` reads the
 * nearest `package.json` first, so an unparsable one would end the script with exit 1 before any
 * check ran. A key that is present but neither an empty object nor an empty array counts
 * as declaring dependencies, because the repo is not in a shape this runner supports.
 */
export const DEPENDENCY_CHECK_SCRIPT = `
const fs = process.getBuiltinModule("fs");
const keys = [
  "dependencies",
  "devDependencies",
  "optionalDependencies",
  "peerDependencies",
  "bundleDependencies",
  "bundledDependencies",
  "workspaces",
];
let pkg;
try {
  pkg = JSON.parse(fs.readFileSync("package.json", "utf8"));
} catch (error) {
  const missing = error.code === "ENOENT";
  process.exit(missing ? ${PACKAGE_JSON_MISSING_EXIT_CODE} : ${PACKAGE_JSON_INVALID_EXIT_CODE});
}
if (typeof pkg !== "object" || pkg === null || Array.isArray(pkg)) {
  process.exit(${PACKAGE_JSON_INVALID_EXIT_CODE});
}
const declares = (value) => {
  if (value === undefined) return false;
  if (Array.isArray(value)) return value.length > 0;
  if (typeof value === "object" && value !== null) return Object.keys(value).length > 0;
  return true;
};
process.exit(keys.some((key) => declares(pkg[key])) ? ${DEPENDENCIES_DECLARED_EXIT_CODE} : 0);
`;
