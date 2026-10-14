import {
  forkName,
  scopeText,
  type Assumer,
  type ReviewCard,
  type ScopeClaimView,
  type ScopeView,
  type Waiter,
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
  var waiting = card.querySelector('li[data-line="granted"]');
  if (b.closed && b.closed.event === "submit_rejected" && waiting) {
    waiting.textContent = "no grant: the claim is back to active, so the waiters stay queued";
    return;
  }
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
function relock(buttons, card) {
  buttons.forEach(function (x) {
    x.disabled = x.dataset.action === "approve" && !card.dataset.approve;
  });
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
      relock(buttons, card);
    })
    .catch(function (e) {
      say(card, "Request failed: " + String(e) + ". Reload to check.", "bad");
      relock(buttons, card);
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
:root { color-scheme: light dark; --bg: #fff; --fg: #1b1f24; --mute: #59626e; --card: #f6f7f9;
  --line: rgb(27 31 36 / .12); --bad: #b3261e; --ok: #1a7f37;
  --on-ok: #fff; --warn: #9a5b00; --warn-bg: #fff3dc; --quote: #eceef1;
  --off-bg: #e4e7eb; --off-fg: #6b7380; --focus: #1b1f24; --field: #8a929c;
  --raised: 0 0 0 1px rgb(0 0 0 / .06), 0 1px 3px rgb(0 0 0 / .05),
    0 4px 10px -4px rgb(0 0 0 / .06);
  --s1: 4px; --s2: 8px; --s3: 12px; --s4: 16px; --s5: 24px; --s6: 32px; }
@media (prefers-color-scheme: dark) { :root { --bg: #14171b; --fg: #e6e8eb; --mute: #a3acb8;
  --card: #1c2025; --line: #323941; --bad: #ff8a80; --ok: #6fd58a;
  --on-ok: #0b2a14; --warn: #f0b429; --warn-bg: #2e2510; --quote: #252a31;
  --off-bg: #262b32; --off-fg: #8a939e; --focus: #e6e8eb; --field: #6c7581;
  --raised: 0 0 0 1px rgb(255 255 255 / .09); } }
*, *::before, *::after { box-sizing: border-box; }
body { margin: 0 auto; padding: 16px; max-width: 40rem; font: 16px/1.5 system-ui, sans-serif;
  background: var(--bg); color: var(--fg); -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale; font-synthesis: none; }
header { margin: var(--s2) 0 var(--s4); }
h1 { font-size: 1rem; font-weight: 600; margin: 0; color: var(--mute); }
.count { margin: var(--s1) 0 0; font-size: 1.25rem; font-weight: 600;
  font-variant-numeric: tabular-nums; }
article { --pad: var(--s5); --radius: 12px; background: var(--card); box-shadow: var(--raised);
  border-radius: var(--radius); padding: var(--pad); margin: var(--s4) 0; min-width: 0; }
@media (max-width: 30rem) { article { --pad: var(--s4); } }
h2 { font-size: 1.75rem; line-height: 1.15; letter-spacing: -.01em; margin: 0;
  font-variant-numeric: tabular-nums; text-wrap: balance; overflow-wrap: anywhere; }
.lede { font-size: 1.125rem; line-height: 1.45; margin: var(--s2) 0; text-wrap: pretty; }
.meta { color: var(--mute); font-size: .875rem; margin: 0; overflow-wrap: anywhere;
  font-variant-numeric: tabular-nums; }
section { margin-top: var(--s5); padding-top: var(--s4); border-top: 1px solid var(--line); }
h3 { font-size: 1.125rem; line-height: 1.3; margin: 0 0 var(--s3); text-wrap: balance;
  overflow-wrap: anywhere; }
h4 { font-size: .875rem; font-weight: 600; color: var(--mute); margin: var(--s4) 0 var(--s2); }
h3 + h4 { margin-top: 0; }
ul, ol { margin: 0; padding: 0; list-style: none; }
li, blockquote, p { overflow-wrap: anywhere; }
li + li { margin-top: var(--s2); }
p { margin: 0; }
code { font: .875em ui-monospace, SFMono-Regular, Menlo, monospace; padding: 1px var(--s1);
  border-radius: 4px; background: var(--quote); overflow-wrap: anywhere;
  font-variant-numeric: tabular-nums; }
.from code, .meta code, .detail code { font-size: .9em; }
.mode code { font-size: 1em; padding: 0; background: none; white-space: normal; }
blockquote { margin: var(--s1) 0 0; padding: var(--s2) var(--s3); background: var(--quote);
  border-radius: 6px; white-space: pre-wrap; }
.from { font-size: .8125rem; color: var(--mute); margin: 0 0 var(--s1); }
.reasons li { position: relative; padding: var(--s3) var(--s3) var(--s3) var(--s6);
  background: var(--warn-bg); border-radius: 8px; }
.reasons li::before { content: ""; position: absolute; left: var(--s3); top: 1.15em;
  width: 10px; height: 10px; border-radius: 50%; background: var(--warn); pointer-events: none; }
.label { font-weight: 600; margin-right: var(--s1); }
.gloss, .detail { display: block; }
.gloss { color: var(--fg); text-wrap: pretty; }
.detail { color: var(--mute); font-size: .875rem; margin-top: var(--s1); }
.touched li { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--s2); }
.mode, .hold { font-size: .8125rem; padding: 0 var(--s2); border-radius: 999px;
  border: 1px solid var(--line); color: var(--mute); white-space: nowrap; }
.hold { border-color: var(--warn); color: var(--warn); }
.touched + .meta { margin-top: var(--s3); }
.callout { padding: var(--s3); background: var(--bg); box-shadow: 0 0 0 1px var(--line);
  border-radius: 8px; }
.receipt { font-variant-numeric: tabular-nums; }
.receipt li { position: relative; padding: 0 0 var(--s4) var(--s6); margin: 0; }
.receipt li::before { content: ""; position: absolute; left: 0; top: .3em; width: 14px;
  height: 14px; border-radius: 50%; border: 2px solid var(--mute); background: var(--card); }
.receipt li:not(:last-child)::after { content: ""; position: absolute; left: 6px;
  top: calc(.3em + 14px); bottom: -.3em; width: 2px; background: var(--line); }
.receipt li.done::before { border-color: var(--ok); background: var(--ok); }
pre { max-height: 60vh; overflow: auto; padding: var(--s2); background: var(--bg);
  border: 1px solid var(--line); border-radius: 6px; font-size: .8125rem; }
.field { display: block; margin: var(--s2) 0; font-size: .875rem; font-weight: 600; }
textarea { display: block; width: 100%; min-height: 44px; font: 16px/1.5 system-ui, sans-serif;
  margin-top: var(--s1); padding: var(--s2); border: 1px solid var(--field); border-radius: 6px;
  background: var(--bg); color: var(--fg); resize: vertical; }
@media (hover: hover) { textarea:hover { border-color: var(--fg); } }
button { font: inherit; min-height: 44px; padding: 6px 14px; border-radius: 6px;
  cursor: pointer; transition: transform 150ms ease-out, background-color 150ms ease-out,
    box-shadow 150ms ease-out, filter 150ms ease-out; }
button:active:not(:disabled) { transform: scale(.97); }
button:focus-visible, textarea:focus-visible { outline: 2px solid var(--focus);
  outline-offset: 2px; }
button[data-action="diff"] { background: var(--fg); color: var(--bg); border: 0;
  font-weight: 600; }
section:has(.diff:not(:empty)) button[data-action="diff"] { background: var(--bg);
  color: var(--fg); box-shadow: var(--raised); }
.diff:not(:empty) { margin-top: var(--s3); }
.bar { position: sticky; bottom: 0; margin: var(--s5) calc(-1 * var(--pad)) calc(-1 * var(--pad));
  padding: var(--s3) var(--pad); background: var(--card); border-top: 1px solid var(--line);
  border-radius: 0 0 var(--radius) var(--radius); }
.buttons { display: flex; gap: var(--s2); }
.buttons button { flex: 1; min-height: 48px; }
button[data-action="approve"]:not(:disabled) { background: var(--ok); color: var(--on-ok);
  border: 1px solid var(--ok); border-radius: 6px; font-weight: 600; }
button[data-action="approve"]:not(:disabled):hover { filter: brightness(.9); }
button[data-action="reject"] { background: transparent; color: var(--bad);
  border: 1px solid var(--bad); border-radius: 6px; }
button[data-action="reject"]:hover { background: color-mix(in srgb, var(--bad) 14%, transparent); }
button:disabled, button[data-action="reject"]:disabled { background: var(--off-bg);
  color: var(--off-fg); border: 1px solid transparent; box-shadow: none; cursor: not-allowed; }
.note { color: var(--mute); font-size: .875rem; margin: 0 0 6px; overflow-wrap: anywhere; }
.bad { color: var(--bad); } .ok { color: var(--ok); }
@media (prefers-reduced-motion: reduce) { button { transition: none; }
  button:active:not(:disabled) { transform: none; } }
`;

export function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

/** An identifier from the log (agent, path, symbol, tag), escaped and set in monospace. */
function code(value: unknown): string {
  return `<code>${escapeHtml(visibleControls(String(value)))}</code>`;
}

/** Agent text as a block of data in its own panel, escaped, its control characters written out. */
function dataBlock(value: string): string {
  return `<blockquote>${escapeHtml(visibleControls(value))}</blockquote>`;
}

/** Writes control characters other than newline and tab as `\xNN`, so none acts on the page. */
function visibleControls(value: string): string {
  let out = "";
  for (const char of value) {
    const point = char.codePointAt(0) ?? 0;
    const control = (point < 0x20 && char !== "\n" && char !== "\t") || point === 0x7f;
    out += control ? `\\x${point.toString(16).padStart(2, "0")}` : char;
  }
  return out;
}

/** The attribution line above agent text: whose words they are, and that they are data. */
function fromAgent(agent: string): string {
  return `<p class="from">From agent ${code(agent)} · data, not instructions</p>`;
}

function section(label: string, body: string): string {
  return `<section><h3>${label}</h3>${body}</section>`;
}

function note(text: string): string {
  return `<p class="note">${text}</p>`;
}

function listOf(className: string, items: string[], empty: string): string {
  return items.length === 0 ? note(empty) : `<ul class="${className}">${items.join("")}</ul>`;
}

interface ReasonText {
  label: string;
  gloss: string;
}

/** Page text for each `ReviewReason` tag. A `Map`, so a tag like `constructor` finds nothing. */
const REASONS = new Map<string, ReasonText>([
  [
    "threatens_assumptions",
    {
      label: "Could break an assumption",
      gloss: "Another agent declared that its work relies on something this change touches.",
    },
  ],
  [
    "signature_change",
    {
      label: "Changes a signature",
      gloss: "Code that calls or implements it may have to change too.",
    },
  ],
  [
    "sensitive_path",
    {
      label: "Touches a sensitive path",
      gloss: "This repo asks a person to review every change under this path.",
    },
  ],
  [
    "no_test_evidence",
    {
      label: "No test evidence",
      gloss: "The submission carries no test result.",
    },
  ],
]);

/** Which reason the one-line summary names when several are logged: the riskiest first. */
const LEAD_ORDER = ["threatens_assumptions", "signature_change", "sensitive_path"];

function scopeOf(reason: Record<string, unknown>): ScopeView | undefined {
  const scope = reason["scope"] as Partial<ScopeView> | undefined;
  return typeof scope?.kind === "string" && typeof scope.path === "string"
    ? (scope as ScopeView)
    : undefined;
}

function threatenedText(count: unknown): string {
  if (typeof count === "number") {
    return `${count} threatened ${count === 1 ? "assumption" : "assumptions"}`;
  }
  return `threatened: ${code(count)}`;
}

function reasonDetails(reason: Record<string, unknown>): string {
  const parts: string[] = [];
  const scope = scopeOf(reason);
  if (scope !== undefined) {
    parts.push(`on ${code(scopeText(scope))}`);
  }
  if (reason["pattern"] !== undefined) {
    parts.push(`sensitive path ${code(reason["pattern"])}`);
  }
  if (reason["count"] !== undefined) {
    parts.push(threatenedText(reason["count"]));
  }
  return parts.length === 0 ? "" : `<span class="detail">${parts.join(" · ")}</span>`;
}

function reasonItem(reason: Record<string, unknown>): string {
  const tag = String(reason["reason"]);
  const known = REASONS.get(tag);
  const label = known?.label ?? "A reason this page does not know";
  const gloss = known === undefined ? "" : `<span class="gloss">${known.gloss}</span>`;
  const details = reasonDetails(reason);
  return `<li><span class="label">${label}</span> ${code(tag)}${gloss}${details}</li>`;
}

/** Assumers other than the submitter: its own other claims are not another agent's. */
function otherAssumers(item: ReviewCard): Assumer[] {
  return item.assumers.filter((assumer) => assumer.agent !== item.agent);
}

function threatSentence(item: ReviewCard, count: unknown): string {
  const assumers = otherAssumers(item);
  const names = [...new Set(assumers.map((assumer) => assumer.agent))];
  if (names.length > 0) {
    const noun = assumers.length === 1 ? "an assumption" : "assumptions";
    return `this change could break ${noun} ${names.map(code).join(", ")} declared.`;
  }
  if (typeof count === "number" && count > 1) {
    return `this change could break ${count} assumptions other agents declared.`;
  }
  return "this change could break an assumption another agent declared.";
}

function leadSentence(item: ReviewCard, reason: Record<string, unknown>): string {
  const scope = scopeOf(reason);
  switch (reason["reason"]) {
    case "threatens_assumptions":
      return threatSentence(item, reason["count"]);
    case "signature_change":
      return scope === undefined
        ? "this change alters a signature other code may rely on."
        : `this change alters the signature of ${code(scopeText(scope))}, ` +
            "which other code may rely on.";
    case "sensitive_path":
      return reason["pattern"] === undefined
        ? "this change touches a path this repo marks sensitive."
        : `this change touches ${code(reason["pattern"])}, a path this repo marks sensitive.`;
    case "no_test_evidence":
      return "no test result is attached to this change.";
    default:
      return `the log carries a reason this page does not know: ${code(reason["reason"])}.`;
  }
}

/** One line in plain words: what the reviewer is asked and why, from the logged reasons only. */
function summaryLine(item: ReviewCard): string {
  const [first] = item.reasons;
  if (first === undefined) {
    return `<p class="lede">Held for review. The log carries no reason.</p>`;
  }
  const lead =
    LEAD_ORDER.map((tag) => item.reasons.find((reason) => reason["reason"] === tag)).find(
      (reason) => reason !== undefined,
    ) ?? first;
  const more = item.reasons.length > 1 ? " The other reasons are listed below." : "";
  return `<p class="lede">Held: ${leadSentence(item, lead)}${more}</p>`;
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

const MODES = new Map<string, string>([
  ["depend", "depends on it"],
  ["edit_body", "edits the body"],
  ["edit_signature", "changes the signature"],
  ["create", "adds new symbols"],
]);

function modeLabel(mode: string): string {
  const known = MODES.get(mode);
  return `<span class="mode">${known ?? code(mode)}</span>`;
}

function touchedItem(touched: ScopeClaimView, tag = ""): string {
  return `<li>${code(scopeText(touched.scope))} ${modeLabel(touched.mode)}${tag}</li>`;
}

/** The distinct scopes the logged reasons name, in the order logged. */
function holdScopes(reasons: Array<Record<string, unknown>>): string[] {
  const out: string[] = [];
  for (const reason of reasons) {
    const scope = scopeOf(reason);
    if (scope !== undefined && !out.includes(scopeText(scope))) {
      out.push(scopeText(scope));
    }
  }
  return out;
}

/** Touched scopes a reason names come first and are tagged; so are named scopes not touched. */
function touchedItems(item: ReviewCard): string[] {
  const holds = holdScopes(item.reasons);
  const tag = ` <span class="hold">a reason it is held</span>`;
  const isHeld = (touched: ScopeClaimView) => holds.includes(scopeText(touched.scope));
  const held = item.touched.filter(isHeld).map((touched) => touchedItem(touched, tag));
  const untouched = holds
    .filter((hold) => !item.touched.some((touched) => scopeText(touched.scope) === hold))
    .map((hold) => `<li>${code(hold)}${tag}</li>`);
  const rest = item.touched.filter((touched) => !isHeld(touched));
  return [...held, ...untouched, ...rest.map((touched) => touchedItem(touched))];
}

function touchesSection(repo: string, item: ReviewCard): string {
  const list = listOf("touched", touchedItems(item), "Nothing listed.");
  const fork = code(forkName(repo, item.agent));
  const pushed = `<p class="meta">Pushed to fork ${fork} at ${code(item.forkCommit)}</p>`;
  return section("What it touches", list + pushed);
}

function ownAssumptions(item: ReviewCard): string {
  const items: string[] = [];
  for (const assumption of item.intent.assumptions) {
    const scope = assumption.scope as Partial<ScopeView> | null;
    if (typeof scope?.kind === "string" && typeof scope.path === "string") {
      const about = `<p>About ${code(scopeText(assumption.scope))}</p>`;
      items.push(`<li>${about}${dataBlock(String(assumption.statement))}</li>`);
    }
  }
  return items.length === 0
    ? ""
    : `<h4>Assumptions it declares</h4><ul class="assumptions">${items.join("")}</ul>`;
}

function agentSection(item: ReviewCard): string {
  const task =
    item.intent.taskRef === null ? "" : `<h4>Task ref</h4>${dataBlock(item.intent.taskRef)}`;
  const evidence = listOf(
    "evidence",
    item.evidence.map((line) => `<li>${dataBlock(line)}</li>`),
    "No evidence attached.",
  );
  return section(
    "What the agent says",
    fromAgent(item.agent) +
      `<h4>Intent</h4>${dataBlock(item.intent.summary)}${task}${ownAssumptions(item)}` +
      `<h4>Evidence</h4>${evidence}`,
  );
}

function assumerCallout(assumer: Assumer): string {
  const who = `${code(assumer.agent)} assumes this about ${code(scopeText(assumer.scope))}:`;
  const statement = dataBlock(assumer.statement);
  return `<li class="callout">${fromAgent(assumer.agent)}<p>${who}</p>${statement}</li>`;
}

function waiterItem(waiter: Waiter): string {
  const place = escapeHtml(String(waiter.position));
  const scope = code(scopeText(waiter.scope));
  return `<li>${code(waiter.agent)} is #${place} in the queue for ${scope}</li>`;
}

function breakSection(item: ReviewCard): string {
  const assumers = listOf(
    "exposed",
    otherAssumers(item).map(assumerCallout),
    "No other agent's live claim declares an assumption about these scopes.",
  );
  const waiters = listOf(
    "waiters",
    item.waiters.map(waiterItem),
    "The log shows no agent queued on these scopes.",
  );
  return section(
    "What it could break",
    `<h4>Assumptions other agents declared</h4>${assumers}<h4>Waiting behind it</h4>${waiters}`,
  );
}

function diffSection(canDiff: boolean): string {
  const body = canDiff
    ? `<button data-action="diff">Load the diff</button><div class="diff"></div>`
    : note("Only a reviewer may start the diff: it runs in a sandbox.");
  return section("The diff", body);
}

function receiptSection(item: ReviewCard): string {
  const names = item.waiters.map((waiter) => escapeHtml(waiter.agent)).join(", ");
  const granted =
    item.waiters.length === 0
      ? ""
      : `<li data-line="granted">${names} granted · waits for the merge</li>`;
  const nobody =
    item.waiters.length === 0 ? note("Nobody was waiting on this claim, so no grant follows.") : "";
  return section(
    "What happens next",
    note("Each line is a logged event; a filled dot means it is in the log.") +
      `<ol class="receipt"><li data-line="decided">review_decided · waits for you</li>` +
      `<li data-line="closed">merged · waits for the steward</li>${granted}</ol>${nobody}` +
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
    `<label class="field">Note for the agent (optional)` +
    `<textarea rows="2" maxlength="900"></textarea></label>` +
    `<div class="buttons"><button data-action="approve" disabled>Approve</button>` +
    `<button data-action="reject">Reject with a note</button></div>` +
    `<div class="result" aria-live="polite"></div></div>`
  );
}

function card(repo: string, item: ReviewCard, rejectToken: string | undefined): string {
  const tokenAttribute =
    rejectToken === undefined ? "" : ` data-reject-csrf="${escapeHtml(rejectToken)}"`;
  const order = `${item.place} of ${item.total} in the queue, sorted by who it unblocks.`;
  const reasons = listOf("reasons", item.reasons.map(reasonItem), "No reason was logged.");
  const unblocks = unblocksText(item.unblocks);
  return (
    `<article data-claim="${item.claim}" data-commit="${escapeHtml(item.forkCommit)}"` +
    `${tokenAttribute}>` +
    `<h2>Claim ${item.claim} · ${escapeHtml(item.agent)} · ${unblocks}</h2>` +
    summaryLine(item) +
    `<p class="meta">${order}${heldForText(item.heldMinutes)}</p>` +
    section("Why you are asked", reasons) +
    touchesSection(repo, item) +
    agentSection(item) +
    breakSection(item) +
    diffSection(rejectToken !== undefined) +
    receiptSection(item) +
    decisionBar(rejectToken) +
    `</article>`
  );
}

export interface ReviewPageInput {
  repo: string;
  nonce: string;
  cards: ReviewCard[];
  /**
   * A reject token per claim, absent for a viewer who may not decide. Approve tokens come with
   * the diff.
   */
  rejectTokens: ReadonlyMap<number, string>;
}

/** The page with its inline script and style bound to `nonce` by a Content-Security-Policy. */
export function reviewPage({ repo, nonce, cards, rejectTokens }: ReviewPageInput): Response {
  const body =
    cards.length === 0
      ? `<p class="note">Nothing is held for review. A submission that needs a person ` +
        `appears here with the reasons it was held.</p>`
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
