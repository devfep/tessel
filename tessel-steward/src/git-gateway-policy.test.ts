import { describe, expect, it } from "vitest";

import { isAllowedGitRequest } from "./git-gateway-policy";

const HOST = "https://1e40d7b5aed4b7049e5b83bc07a5264c.artifacts.cloudflare.net";
const REMOTE = `${HOST}/git/tessel/demo.git`;
const INFO_REFS = `${REMOTE}/info/refs?service=git-upload-pack`;
const UPLOAD_PACK = `${REMOTE}/git-upload-pack`;

function allowed(method: string, url: string): boolean {
  return isAllowedGitRequest({ method, url }, REMOTE);
}

describe("isAllowedGitRequest", () => {
  it("allows the ref advertisement of the run's repo", () => {
    expect(allowed("GET", INFO_REFS)).toBe(true);
  });

  it("allows the upload-pack POST of the run's repo", () => {
    expect(allowed("POST", UPLOAD_PACK)).toBe(true);
  });

  it.each([
    ["another repo", "GET", `${HOST}/git/tessel/other.git/info/refs?service=git-upload-pack`],
    ["another namespace", "GET", `${HOST}/git/other/demo.git/info/refs?service=git-upload-pack`],
    [
      "a repo whose name extends the allowed one",
      "POST",
      `${HOST}/git/tessel/demo.git2/git-upload-pack`,
    ],
    ["a repo path nested below the allowed one", "POST", `${REMOTE}/x/git-upload-pack`],
    [
      "another host",
      "GET",
      "https://example.com/git/tessel/demo.git/info/refs?service=git-upload-pack",
    ],
    [
      "another account's artifacts host",
      "POST",
      "https://abc.artifacts.cloudflare.net/git/tessel/demo.git/git-upload-pack",
    ],
    [
      "a host with the allowed one as a prefix",
      "GET",
      `${HOST}.evil.test/git/tessel/demo.git/info/refs?service=git-upload-pack`,
    ],
    [
      "a non-default port",
      "GET",
      `${HOST}:8443/git/tessel/demo.git/info/refs?service=git-upload-pack`,
    ],
    ["plain http", "GET", INFO_REFS.replace("https:", "http:")],
    ["a push advertisement", "GET", `${REMOTE}/info/refs?service=git-receive-pack`],
    ["a push", "POST", `${REMOTE}/git-receive-pack`],
    ["a missing query string", "GET", `${REMOTE}/info/refs`],
    ["an extra query parameter", "GET", `${INFO_REFS}&x=1`],
    ["a different service value", "GET", `${REMOTE}/info/refs?service=git-upload-archive`],
    ["a query string on the POST", "POST", `${UPLOAD_PACK}?service=git-upload-pack`],
    [
      "a path traversal into another repo",
      "GET",
      `${REMOTE}/../other.git/info/refs?service=git-upload-pack`,
    ],
    ["the remote root", "GET", REMOTE],
  ])("denies %s", (_label, method, url) => {
    expect(allowed(method, url)).toBe(false);
  });

  it.each(["PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "get"])("denies method %s", (method) => {
    expect(allowed(method, INFO_REFS)).toBe(false);
    expect(allowed(method, UPLOAD_PACK)).toBe(false);
  });

  it("denies GET on the upload-pack path and POST on the info/refs path", () => {
    expect(allowed("GET", UPLOAD_PACK)).toBe(false);
    expect(allowed("POST", INFO_REFS)).toBe(false);
  });
});
