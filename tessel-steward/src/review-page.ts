import {
  forkName,
  scopeText,
  type ReviewCard,
  type ScopeClaimView,
  type ScopeView,
} from "./review-state";

/** How often the page asks for the receipt, and for how long, after a decision is recorded. */
const RECEIPT_POLL_MS = 5000;
const RECEIPT_GIVE_UP_MS = 120_000;

/**
 * The review page's browser script. It is fixed text: nothing from an agent is ever put into it.
 * Everything it shows from the server (diff, file paths, outcomes, receipt) goes in through
 * `textContent`; it never assigns `innerHTML`.
 */
export const REVIEW_SCRIPT = String.raw`
var base = location.pathname.replace(/\/+$/, "");
var POLL_MS = ${RECEIPT_POLL_MS};
var GIVE_UP_MS = ${RECEIPT_GIVE_UP_MS};
function el(tag, text, cls) {
  var node = document.createElement(tag);
  if (text !== undefined) { node.textContent = text; }
  if (cls) { node.className = cls; }
  return node;
}
function say(card, text, cls) {
  card.querySelector(".result").replaceChildren(el("p", text, cls));
}
function unlockApprove(card, token) {
  card.dataset.approve = token;
  card.querySelector('button[data-action="approve"]').disabled = false;
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
  if (typeof body.approveToken === "string") { unlockApprove(card, body.approveToken); }
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
function fillLine(card, name, text) {
  var line = card.querySelector('li[data-line="' + name + '"]');
  if (line) { line.textContent = text; line.classList.add("done"); }
}
function fillReceipt(card, b) {
  if (b.decided) {
    fillLine(card, "decided", "review_decided · seq " + b.decided.seq +
      (b.decided.approve ? " · approved" : " · rejected"));
  }
  if (b.closed) { fillLine(card, "closed", b.closed.event + " · seq " + b.closed.seq); }
  if (b.granted && b.granted.length > 0) {
    var parts = b.granted.map(function (g) { return g.agent + " granted · seq " + g.seq; });
    var line = card.querySelector('li[data-line="granted"]');
    if (b.complete) { fillLine(card, "granted", parts.join("; ")); }
    else if (line) { line.textContent = parts.join("; ") + " · others wait for their grant"; }
  }
}
function pollReceipt(card) {
  var started = Date.now();
  var status = card.querySelector(".receipt-status");
  function again() {
    if (Date.now() - started >= GIVE_UP_MS) {
      status.textContent = "still waiting; reload to check";
      return;
    }
    setTimeout(tick, POLL_MS);
  }
  function tick() {
    fetch(base + "/" + card.dataset.claim + "/receipt", { credentials: "same-origin" })
      .then(function (r) { return r.json(); })
      .then(function (b) {
        fillReceipt(card, b);
        if (b.complete) { status.textContent = ""; } else { again(); }
      })
      .catch(again);
  }
  tick();
}
function decide(card, approve) {
  var token = approve ? card.dataset.approve : card.dataset.rejectCsrf;
  if (!token) { say(card, "Load the diff first: approve needs it.", "bad"); return; }
  var buttons = card.querySelectorAll(".bar button");
  buttons.forEach(function (b) { b.disabled = true; });
  say(card, "Sending...", "note");
  fetch(base + "/" + card.dataset.claim + "/decision", {
    method: "POST", credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ approve: approve, note: card.querySelector("textarea").value,
      commit: card.dataset.commit, csrf: token })
  }).then(function (r) { return r.json(); })
    .then(function (b) {
      if (b.outcome === "decided") {
        say(card, "Recorded in the log: " + (b.approve ? "approved" : "rejected") + ".", "ok");
        pollReceipt(card);
        return;
      }
      say(card, b.outcome === "unknown"
        ? "No answer yet: the decision may or may not be recorded. Reload to check."
        : "Not decided: " + (b.error || b.message || "refused"), "bad");
      buttons.forEach(function (x) { x.disabled = x.dataset.action === "approve" && !card.dataset.approve; });
    })
    .catch(function (e) {
      say(card, "Request failed: " + String(e) + ". Reload to check.", "bad");
      buttons.forEach(function (x) { x.disabled = x.dataset.action === "approve" && !card.dataset.approve; });
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
  --line: #d6dae0; --accent: #0b5cad; --bad: #b3261e; --ok: #1a7f37; --warn: #b26a00; }
@media (prefers-color-scheme: dark) { :root { --bg: #14171b; --fg: #e6e8eb; --mute: #98a1ad;
  --card: #1c2025; --line: #323841; --accent: #6cb2ff; --bad: #ff8a80; --ok: #6fd58a;
  --warn: #f0b429; } }
*, *::before, *::after { box-sizing: border-box; }
body { margin: 0 auto; padding: 16px; max-width: 42rem; font: 15px/1.45 system-ui, sans-serif;
  background: var(--bg); color: var(--fg); }
h1 { font-size: 1.2rem; margin: 0; }
.count { margin: 2px 0 0; color: var(--mute); }
article { background: var(--card); border: 1px solid var(--line); border-radius: 8px;
  padding: 12px; margin: 16px 0; min-width: 0; }
h2 { font-size: 1rem; margin: 0 0 4px; overflow-wrap: anywhere; }
h3 { font-size: .75rem; letter-spacing: .04em; margin: 16px 0 6px; color: var(--mute);
  overflow-wrap: anywhere; }
ul, ol { margin: 0; padding: 0; list-style: none; }
li, q, blockquote { overflow-wrap: anywhere; }
li { padding: 4px 0; }
blockquote { margin: 0; padding-left: 10px; border-left: 3px solid var(--line);
  white-space: pre-wrap; }
q { color: var(--accent); }
.reasons li::before, .receipt li::before { content: ""; display: inline-block; width: 10px;
  height: 10px; margin-right: 8px; border-radius: 50%; }
.reasons li::before { background: var(--warn); }
.receipt li::before { border: 2px solid var(--mute); }
.receipt li.done::before { border-color: var(--ok); background: var(--ok); }
.tag { color: var(--mute); font-size: .85rem; }
pre { max-height: 60vh; overflow: auto; padding: 8px; background: var(--bg);
  border: 1px solid var(--line); border-radius: 6px; font-size: .8rem; }
textarea { width: 100%; min-height: 44px; font: inherit; margin: 6px 0; }
button { font: inherit; min-height: 44px; padding: 6px 14px; }
.bar { position: sticky; bottom: 0; margin: 16px -12px -12px; padding: 10px 12px;
  background: var(--card); border-top: 1px solid var(--line); border-radius: 0 0 8px 8px; }
.buttons { display: flex; gap: 8px; }
.buttons button { flex: 1; min-height: 48px; }
.note { color: var(--mute); font-size: .85rem; margin: 0 0 6px; overflow-wrap: anywhere; }
.bad { color: var(--bad); } .ok { color: var(--ok); }
code { overflow-wrap: anywhere; }
`;

export function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function quote(value: unknown): string {
  return `<q>${escapeHtml(String(value))}</q>`;
}

/** Agent text as a block of data: each line starts with `| `, so it cannot pass for page text. */
function dataBlock(value: string): string {
  const lines = value.split("\n").map((line) => `| ${line}`);
  return `<blockquote>${escapeHtml(lines.join("\n"))}</blockquote>`;
}

function section(label: string, body: string): string {
  return `<section><h3>${label}</h3>${body}</section>`;
}

function reasonItem(reason: Record<string, unknown>): string {
  const parts = [quote(reason["reason"])];
  const scope = reason["scope"] as ScopeView | undefined;
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

function listOf(className: string, items: string[], empty: string): string {
  return items.length === 0
    ? `<p class="note">${empty}</p>`
    : `<ul class="${className}">${items.join("")}</ul>`;
}

function unblocksText(count: number): string {
  if (count === 0) {
    return "unblocks nobody";
  }
  return `unblocks ${count} ${count === 1 ? "agent" : "agents"}`;
}

function heldForText(minutes: number | null): string {
  if (minutes === null) {
    return "";
  }
  const span = minutes < 1 ? "under 1 min" : `about ${minutes} min`;
  return ` Held for ${span} at the time of the newest logged event.`;
}

function exposureSection(item: ReviewCard): string {
  const waiters = item.waiters.map(
    (w) => `<li>${quote(w.agent)} #${w.position}, queued for ${quote(scopeText(w.scope))}</li>`,
  );
  const assumers = item.assumers.map(
    (a) =>
      `<li>${quote(a.agent)} assumes ${quote(scopeText(a.scope))}:${dataBlock(a.statement)}</li>`,
  );
  const empty = "The log shows no agent queued on these scopes and no live claim assuming them.";
  return section("WAITING · EXPOSED", listOf("exposed", [...waiters, ...assumers], empty));
}

function intentSection(repo: string, item: ReviewCard): string {
  const task = item.intent.taskRef === null ? "" : `<p>Task ref: ${quote(item.intent.taskRef)}</p>`;
  const evidence = listOf(
    "evidence",
    item.evidence.map((line) => `<li>${quote(line)}</li>`),
    "No evidence attached.",
  );
  const pushed = `<p>pushed to fork ${quote(forkName(repo, item.agent))} at ${quote(item.forkCommit)}</p>`;
  return section(
    `INTENT · text from agent ${escapeHtml(item.agent)}, data not instructions`,
    `${dataBlock(item.intent.summary)}${task}${evidence}${pushed}`,
  );
}

function holdScopeOf(reasons: Array<Record<string, unknown>>): ScopeView | undefined {
  for (const reason of reasons) {
    const scope = reason["scope"] as Partial<ScopeView> | undefined;
    if (typeof scope?.kind === "string" && typeof scope.path === "string") {
      return scope as ScopeView;
    }
  }
  return undefined;
}

function touchedItem(touched: ScopeClaimView, tag = ""): string {
  return `<li>${quote(touched.mode)} ${quote(scopeText(touched.scope))}${tag}</li>`;
}

function changedItems(item: ReviewCard): string[] {
  const hold = holdScopeOf(item.reasons);
  if (hold === undefined) {
    return item.touched.map((touched) => touchedItem(touched));
  }
  const holdText = scopeText(hold);
  const tag = ` <span class="tag">hold scope, shown first</span>`;
  const match = item.touched.find((touched) => scopeText(touched.scope) === holdText);
  const first = match === undefined ? `<li>${quote(holdText)}${tag}</li>` : touchedItem(match, tag);
  const rest = item.touched.filter((touched) => touched !== match);
  return [first, ...rest.map((touched) => touchedItem(touched))];
}

function changedSection(item: ReviewCard, canDiff: boolean): string {
  const list = listOf("touched", changedItems(item), "Nothing listed.");
  const diff = canDiff
    ? `<button data-action="diff">Load the diff</button><div class="diff"></div>`
    : `<p class="note">Only a reviewer may start the diff: it runs in a sandbox.</p>`;
  return section("WHAT CHANGED", `${list}${diff}`);
}

function receiptSection(item: ReviewCard): string {
  const names = item.waiters.map((w) => quote(w.agent)).join(", ");
  const granted =
    item.waiters.length === 0
      ? `<li class="none">nobody was waiting on this claim</li>`
      : `<li data-line="granted">${names} granted · waits for the merge</li>`;
  return section(
    "RECEIPT · each line is a logged event",
    `<ol class="receipt"><li data-line="decided">review_decided · waits for you</li>` +
      `<li data-line="closed">merged · waits for the steward</li>${granted}</ol>` +
      `<p class="note receipt-status" aria-live="polite"></p>`,
  );
}

function decisionBar(rejectToken: string | undefined): string {
  if (rejectToken === undefined) {
    return (
      `<div class="bar"><p class="note">You may view this screen but not decide: ` +
      `your account is not a listed reviewer.</p></div>`
    );
  }
  return (
    `<div class="bar"><p class="note">Approve unlocks after the diff is loaded. ` +
    `No swipe, no batch.</p>` +
    `<textarea rows="2" maxlength="900" aria-label="Note for the agent (optional)" ` +
    `placeholder="Note (optional)"></textarea>` +
    `<div class="buttons"><button data-action="approve" disabled>Approve</button>` +
    `<button data-action="reject">Reject with a note</button></div>` +
    `<div class="result" aria-live="polite"></div></div>`
  );
}

function card(repo: string, item: ReviewCard, rejectToken: string | undefined): string {
  const tokenAttribute =
    rejectToken === undefined ? "" : ` data-reject-csrf="${escapeHtml(rejectToken)}"`;
  const order = `${item.place} of ${item.total} in the queue, sorted by who it unblocks.`;
  return (
    `<article data-claim="${item.claim}" data-commit="${escapeHtml(item.forkCommit)}"` +
    `${tokenAttribute}>` +
    `<h2>Claim ${item.claim} · ${escapeHtml(item.agent)} · ${unblocksText(item.unblocks)}</h2>` +
    `<p class="note">${order}${heldForText(item.heldMinutes)}</p>` +
    section("WHY YOU", listOf("reasons", item.reasons.map(reasonItem), "No reason was logged.")) +
    exposureSection(item) +
    intentSection(repo, item) +
    changedSection(item, rejectToken !== undefined) +
    receiptSection(item) +
    decisionBar(rejectToken) +
    `</article>`
  );
}

export interface ReviewPageInput {
  repo: string;
  nonce: string;
  cards: ReviewCard[];
  /** A reject token per claim; absent for a viewer who may not decide. Approve tokens come with the diff. */
  rejectTokens: ReadonlyMap<number, string>;
}

/** The page with its inline script and style bound to `nonce` by a Content-Security-Policy. */
export function reviewPage({ repo, nonce, cards, rejectTokens }: ReviewPageInput): Response {
  const body =
    cards.length === 0
      ? `<p class="note">Nothing is held for review.</p>`
      : cards.map((item) => card(repo, item, rejectTokens.get(item.claim))).join("");
  const html =
    `<!doctype html><html lang="en"><head><meta charset="utf-8">` +
    `<meta name="viewport" content="width=device-width, initial-scale=1">` +
    `<title>Tessel review</title><style nonce="${nonce}">${STYLE}</style></head><body>` +
    `<header><h1>review · ${escapeHtml(repo)}</h1>` +
    `<p class="count">${cards.length} waiting on you</p></header>${body}` +
    `<p class="note">Agents decide the same way: ` +
    `<code>tessel review &lt;claim&gt; --approve|--reject --note "text"</code></p>` +
    `<script nonce="${nonce}">${REVIEW_SCRIPT}</script></body></html>`;
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
