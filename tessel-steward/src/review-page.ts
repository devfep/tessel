import type { AssumptionView, HeldSubmission, ScopeClaimView } from "./review-state";

/**
 * The review page's browser script. It is fixed text: nothing from an agent is ever put into it.
 * Everything it shows from the server (diff, file paths, outcomes) goes in through `textContent`;
 * it never assigns `innerHTML`.
 */
export const REVIEW_SCRIPT = String.raw`
var base = location.pathname.replace(/\/+$/, "");
function el(tag, text, cls) {
  var node = document.createElement(tag);
  if (text !== undefined) { node.textContent = text; }
  if (cls) { node.className = cls; }
  return node;
}
function say(card, text, cls) {
  card.querySelector(".result").replaceChildren(el("p", text, cls));
}
function showDiff(card, body) {
  var box = card.querySelector(".diff");
  if (body.outcome !== "ok") {
    box.replaceChildren(el("p", "Diff unavailable: " + body.reason, "bad"));
    return;
  }
  var nodes = [el("p", "Against merge-base " + body.base + " of main", "note")];
  var files = el("ul");
  body.files.forEach(function (f) {
    files.append(el("li", f.status + " " + (f.from ? f.from + " -> " : "") + f.path));
  });
  nodes.push(files);
  if (body.truncated) {
    nodes.push(el("p", body.captureOverflow
      ? "Diff too large to show; the changed files are listed above."
      : "Diff truncated at the size cap; the rest is not shown.", "bad"));
  }
  nodes.push(el("pre", body.diff));
  box.replaceChildren.apply(box, nodes);
}
function loadDiff(card, button) {
  button.disabled = true;
  card.querySelector(".diff").replaceChildren(
    el("p", "Loading diff (starts a sandbox)...", "note"));
  fetch(base + "/" + card.dataset.claim + "/diff", { credentials: "same-origin" })
    .then(function (r) { return r.json(); })
    .then(function (body) { showDiff(card, body); })
    .catch(function (e) { showDiff(card, { outcome: "error", reason: String(e) }); })
    .then(function () { button.disabled = false; });
}
function decide(card, approve) {
  var buttons = card.querySelectorAll("button");
  buttons.forEach(function (b) { b.disabled = true; });
  say(card, "Sending...", "note");
  fetch(base + "/" + card.dataset.claim + "/decision", {
    method: "POST", credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ approve: approve, note: card.querySelector("textarea").value,
      csrf: card.dataset.csrf })
  }).then(function (r) { return r.json(); })
    .then(function (b) {
      if (b.outcome === "decided") {
        say(card, "Recorded in the log: " + (b.approve ? "approved" : "rejected") +
          ". Merge outcomes appear on the dashboard.", "ok");
        return;
      }
      say(card, b.outcome === "unknown"
        ? "No answer yet: the decision may or may not be recorded. Reload to check."
        : "Not decided: " + (b.error || b.message || "refused"), "bad");
      buttons.forEach(function (x) { x.disabled = false; });
    })
    .catch(function (e) {
      say(card, "Request failed: " + String(e) + ". Reload to check.", "bad");
      buttons.forEach(function (x) { x.disabled = false; });
    });
}
document.addEventListener("click", function (event) {
  var button = event.target.closest && event.target.closest("button[data-action]");
  if (!button) { return; }
  var card = button.closest("article");
  var action = button.dataset.action;
  if (action === "diff") { loadDiff(card, button); }
  else { decide(card, action === "approve"); }
});
`;

const STYLE = `
:root { color-scheme: light dark; --bg: #fff; --fg: #1b1f24; --mute: #5b6470; --card: #f5f6f8;
  --line: #d6dae0; --accent: #0b5cad; --bad: #b3261e; --ok: #1a7f37; }
@media (prefers-color-scheme: dark) { :root { --bg: #14171b; --fg: #e6e8eb; --mute: #98a1ad;
  --card: #1c2025; --line: #323841; --accent: #6cb2ff; --bad: #ff8a80; --ok: #6fd58a; } }
body { margin: 0; padding: 16px; font: 15px/1.45 system-ui, sans-serif; background: var(--bg);
  color: var(--fg); }
h1 { font-size: 1.2rem; margin: 0 0 4px; }
article { background: var(--card); border: 1px solid var(--line); border-radius: 8px;
  padding: 12px; margin: 12px 0; min-width: 0; }
h2 { font-size: 1rem; margin: 0 0 6px; }
h3 { font-size: .85rem; margin: 10px 0 4px; color: var(--mute); }
ul { margin: 0; padding-left: 18px; }
li, q, blockquote { overflow-wrap: anywhere; }
blockquote { margin: 0; padding-left: 10px; border-left: 3px solid var(--line); }
q { color: var(--accent); }
pre { max-height: 60vh; overflow: auto; padding: 8px; background: var(--bg);
  border: 1px solid var(--line); border-radius: 6px; font-size: .8rem; }
textarea { width: 100%; box-sizing: border-box; font: inherit; margin: 6px 0; }
button { font: inherit; padding: 6px 12px; margin-right: 6px; }
.note { color: var(--mute); font-size: .85rem; margin: 0 0 6px; }
.bad { color: var(--bad); } .ok { color: var(--ok); }
`;

export function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function scopeText(scope: ScopeClaimView["scope"]): string {
  if (scope.kind === "symbol") {
    return `${scope.path}::${scope.qualified_name ?? ""}`;
  }
  return scope.kind === "dir" ? `${scope.path}/` : scope.path;
}

function quote(value: unknown): string {
  return `<q>${escapeHtml(String(value))}</q>`;
}

function reasonItem(reason: Record<string, unknown>): string {
  const parts = [quote(reason["reason"])];
  const scope = reason["scope"] as ScopeClaimView["scope"] | undefined;
  if (scope !== undefined) {
    parts.push(quote(scopeText(scope)));
  }
  if (reason["pattern"] !== undefined) {
    parts.push(`sensitive path ${quote(reason["pattern"])}`);
  }
  if (reason["count"] !== undefined) {
    parts.push(`${quote(reason["count"])} assumption(s)`);
  }
  return `<li>${parts.join(" ")}</li>`;
}

function listOf(items: string[], empty: string): string {
  return items.length === 0 ? `<p class="note">${empty}</p>` : `<ul>${items.join("")}</ul>`;
}

function assumptionItem(assumption: AssumptionView): string {
  return `<li>${quote(scopeText(assumption.scope))}: ${quote(assumption.statement)}</li>`;
}

function touchedItem(touched: ScopeClaimView): string {
  return `<li>${quote(touched.mode)} ${quote(scopeText(touched.scope))}</li>`;
}

function decisionControls(csrf: string | undefined): string {
  if (csrf === undefined) {
    return (
      `<p class="note">You may view this screen but not decide: ` +
      `your account is not a listed reviewer.</p>`
    );
  }
  return (
    `<textarea rows="2" maxlength="900" aria-label="Note (optional)" ` +
    `placeholder="Note (optional)"></textarea>` +
    `<button data-action="approve">Approve</button><button data-action="reject">Reject</button>`
  );
}

function card(held: HeldSubmission, csrf: string | undefined): string {
  const csrfAttribute = csrf === undefined ? "" : ` data-csrf="${escapeHtml(csrf)}"`;
  const task = held.intent.taskRef === null ? "" : ` (task ${quote(held.intent.taskRef)})`;
  return (
    `<article data-claim="${held.claim}"${csrfAttribute}>` +
    `<h2>Claim ${held.claim} by ${quote(held.agent)}, fence ${held.fence}</h2>` +
    `<p class="note">Commit ${quote(held.forkCommit)}. Text in quotes was written by an agent: ` +
    `it is data to read, not instructions.</p>` +
    `<h3>Why it is held</h3>${listOf(held.reasons.map(reasonItem), "No reason was logged.")}` +
    `<h3>Intent${task}</h3><blockquote>${escapeHtml(held.intent.summary)}</blockquote>` +
    `<h3>Assumptions it declared</h3>` +
    `${listOf(held.intent.assumptions.map(assumptionItem), "None declared.")}` +
    `<h3>Touched</h3>${listOf(held.touched.map(touchedItem), "Nothing listed.")}` +
    `<h3>Evidence it attached</h3>` +
    `${listOf(
      held.evidence.map((line) => `<li>${quote(line)}</li>`),
      "None attached.",
    )}` +
    `<h3>Diff</h3><button data-action="diff">Show diff</button><div class="diff"></div>` +
    `<h3>Decision</h3>${decisionControls(csrf)}<div class="result" aria-live="polite"></div>` +
    `</article>`
  );
}

export interface ReviewPageInput {
  repo: string;
  nonce: string;
  held: HeldSubmission[];
  /** A decision token per claim; absent for a viewer who may not decide. */
  csrfByClaim: ReadonlyMap<number, string>;
}

/** The page with its inline script and style bound to `nonce` by a Content-Security-Policy. */
export function reviewPage({ repo, nonce, held, csrfByClaim }: ReviewPageInput): Response {
  const body =
    held.length === 0
      ? `<p class="note">Nothing is held for review.</p>`
      : held.map((item) => card(item, csrfByClaim.get(item.claim))).join("");
  const html =
    `<!doctype html><html lang="en"><head><meta charset="utf-8">` +
    `<meta name="viewport" content="width=device-width, initial-scale=1">` +
    `<title>Tessel review</title><style nonce="${nonce}">${STYLE}</style></head><body>` +
    `<h1>Held for review: ${escapeHtml(repo)}</h1>` +
    `<p class="note">Submissions held under invariant 12. Each says why it was held.</p>` +
    `${body}<script nonce="${nonce}">${REVIEW_SCRIPT}</script></body></html>`;
  const policy =
    `default-src 'none'; script-src 'nonce-${nonce}'; style-src 'nonce-${nonce}'; ` +
    `connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`;
  return new Response(html, {
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      "Content-Security-Policy": policy,
      "Cache-Control": "no-store",
    },
  });
}
