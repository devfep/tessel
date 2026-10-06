/**
 * The dashboard's browser script. It runs with `document`, `EventSource`, `fetch`, `location` and
 * `setTimeout` as parameters so that tests can run it against fakes.
 *
 * Every string that came from an agent (intents, assumptions, paths, notes) reaches the page
 * through `textContent` only: this file must never assign `innerHTML` or build markup from data.
 * The headline numbers are shown exactly as `/summary` returns them and are never counted here.
 */
export const PAGE_SCRIPT = String.raw`
var base = location.pathname.replace(/\/+$/, "");
var state = { claims: {}, agentOf: {}, denials: [], waits: [], races: {}, reviews: {},
  merges: [], verified: [], feed: [] };
var timer = 0;
var summaryTimer = 0;

function el(tag, text, cls) {
  var node = document.createElement(tag);
  if (text !== undefined) { node.textContent = text; }
  if (cls) { node.className = cls; }
  return node;
}
function put(id, nodes, empty) {
  var box = document.getElementById(id);
  box.replaceChildren.apply(box, nodes.length ? nodes : [el("p", empty, "empty")]);
}
function pathOf(scope) {
  if (scope.kind === "symbol") { return scope.path + "::" + scope.qualified_name; }
  return scope.path + (scope.kind === "dir" ? "/" : "");
}
function scopesText(scopes) {
  return (scopes || []).map(function (s) { return s.mode + " " + pathOf(s.scope); }).join(", ");
}
function row(parts) {
  var li = el("li");
  parts.forEach(function (p) { li.append(typeof p === "string" ? el("span", p) : p); });
  return li;
}
function quote(text) { return el("q", text); }
function reasonText(r) {
  var detail = r.scope ? pathOf(r.scope) : "";
  if (r.pattern !== undefined) { detail += " (sensitive path " + r.pattern + ")"; }
  if (r.count !== undefined) { detail = r.count + " assumption(s)"; }
  return r.reason + (detail ? ": " + detail : "");
}
function verdictText(outcome) {
  if (outcome === "clean") { return "false alarm (clean): the denied work merged cleanly"; }
  if (outcome === "inconclusive") { return "inconclusive (not counted)"; }
  if (outcome === "textual_conflict" || outcome === "build_failed" || outcome === "tests_failed") {
    return "conflict verified (" + outcome + ")";
  }
  return "unrecognized outcome (" + outcome + ")";
}

function apply(ev) {
  var k = ev.event;
  if (k === "claim_granted") {
    state.claims[ev.claim] = { agent: ev.agent, scopes: ev.scopes, intent: ev.intent, race: ev.race };
    state.agentOf[ev.claim] = ev.agent;
    var at = state.waits.findIndex(function (w) { return w.agent === ev.agent; });
    if (at >= 0) { state.waits.splice(at, 1); }
  } else if (k === "claim_amended" && state.claims[ev.claim]) {
    state.claims[ev.claim].scopes = state.claims[ev.claim].scopes.concat(ev.added);
  } else if (k === "claim_released") {
    delete state.claims[ev.claim];
  } else if (k === "claim_denied" || k === "claim_shadowed") {
    if (k === "claim_shadowed") { state.agentOf[ev.claim] = ev.agent; }
    state.denials.unshift({ agent: ev.agent, scopes: ev.scopes, intent: ev.intent,
      conflicts: ev.conflicts, shadowed: k === "claim_shadowed" });
  } else if (k === "wait_queued") {
    state.waits.push({ agent: ev.agent, req: ev.req, scopes: ev.scopes, intent: ev.intent,
      position: ev.position });
  } else if (k === "wait_withdrawn") {
    state.waits = state.waits.filter(function (w) { return w.req !== ev.req; });
  } else if (k === "race_opened") {
    state.races[ev.race] = { scopes: ev.scopes, decided: false };
  } else if (k === "race_decided") {
    var race = state.races[ev.race] || { scopes: [] };
    race.decided = true; race.winner = ev.winner;
    state.races[ev.race] = race;
  } else if (k === "review_requested") {
    state.reviews[ev.claim] = ev.reasons;
  } else if (k === "review_decided") {
    delete state.reviews[ev.claim];
  } else if (k === "merged") {
    delete state.reviews[ev.claim];
    state.merges.unshift({ claim: ev.claim, head: ev.head, agent: state.agentOf[ev.claim] });
  } else if (k === "denial_verified") {
    state.verified.unshift({ shadow: ev.shadow_claim, blocking: ev.blocking_claim,
      outcome: ev.outcome });
  }
  state.feed.unshift("#" + ev.seq + " " + k + (ev.agent ? " " + ev.agent : "") +
    (ev.claim !== undefined ? " claim " + ev.claim : ""));
  if (state.feed.length > 200) { state.feed.pop(); }
}

function render() {
  put("claims", Object.keys(state.claims).map(function (id) {
    var c = state.claims[id];
    return row([el("strong", c.agent), " holds claim " + id + ": " + scopesText(c.scopes) + " ",
      quote(c.intent.summary)].concat(c.race !== null && c.race !== undefined ? [" (race " + c.race + ")"] : []));
  }), "No active claims.");
  put("denials", state.denials.map(function (d) {
    var parts = [el("strong", d.agent), (d.shadowed ? " DENIED (continues in a shadow fork): " : " DENIED: ") +
      scopesText(d.scopes)];
    d.conflicts.forEach(function (c) {
      parts.push(el("div", "held by " + c.held_by + " (" + scopesText([c.held]) + "), whose intent: "));
      parts[parts.length - 1].append(quote(c.their_intent.summary));
    });
    return row(parts);
  }), "No denials.");
  put("verified", state.verified.map(function (v) {
    return row(["blocked claim " + v.shadow + " vs claim " + v.blocking + ": " + verdictText(v.outcome)]);
  }), "Nothing verified yet.");
  put("waits", state.waits.map(function (w) {
    return row([el("strong", w.agent), " waiting at position " + w.position + " for " +
      scopesText(w.scopes) + " ", quote(w.intent.summary)]);
  }), "Nobody is waiting.");
  put("races", Object.keys(state.races).map(function (id) {
    var r = state.races[id];
    var outcome = !r.decided ? "open" : r.winner === null || r.winner === undefined ?
      "decided: no winner" : "decided: claim " + r.winner + " won";
    return row(["race " + id + " on " + scopesText(r.scopes) + ": " + outcome]);
  }), "No races.");
  put("reviews", Object.keys(state.reviews).map(function (id) {
    return row(["claim " + id + " is held for a human: " + state.reviews[id].map(reasonText).join("; ")]);
  }), "No submissions held for review.");
  put("merges", state.merges.map(function (m) {
    return row(["claim " + m.claim + (m.agent ? " by " + m.agent : "") + " merged, head " + m.head]);
  }), "No merges yet.");
  put("feed", state.feed.map(function (line) { return row([line]); }), "No events yet.");
}

function schedule() {
  if (!timer) { timer = setTimeout(function () { timer = 0; render(); }, 50); }
}
function stat(label, value) {
  var box = el("div", undefined, "stat");
  box.append(el("span", String(value), "num"), el("span", label, "label"));
  return box;
}
function showSummary(body) {
  var s = body.summary;
  var precision = s.precision === null || s.precision === undefined ? "n/a" :
    Math.round(s.precision * 1000) / 10 + "%";
  put("summary", [stat("Claims granted", s.claims_granted), stat("Denials", s.denials),
    stat("Conflicts prevented (verified)", s.conflicts_prevented_verified),
    stat("False alarms", s.false_alarms), stat("Precision", precision),
    stat("Merges", s.merges), stat("Reviews requested", s.reviews_requested)], "No summary.");
}
function loadSummary() {
  fetch(base + "/summary").then(function (r) {
    if (!r.ok) { throw new Error("summary answered " + r.status); }
    return r.json();
  }).then(showSummary).catch(function (e) {
    document.getElementById("status").textContent = "Summary unavailable: " + e.message;
  });
}
function scheduleSummary() {
  if (!summaryTimer) { summaryTimer = setTimeout(function () { summaryTimer = 0; loadSummary(); }, 3000); }
}

document.getElementById("repo").textContent = base.split("/").pop();
var source = new EventSource(base + "/events");
source.onopen = function () { document.getElementById("status").textContent = "Live"; };
source.onerror = function () { document.getElementById("status").textContent = "Reconnecting"; };
source.addEventListener("upstream-error", function (e) {
  document.getElementById("status").textContent = "Coordinator: " + e.data;
});
source.onmessage = function (m) {
  apply(JSON.parse(m.data));
  schedule();
  scheduleSummary();
};
loadSummary();
`;

const STYLE = `
:root { color-scheme: light dark; --bg: #f6f6f4; --card: #fff; --ink: #1d1d1b; --mute: #66665f;
  --line: #dcdcd6; --accent: #b4540a; }
@media (prefers-color-scheme: dark) { :root { --bg: #151514; --card: #1f1f1d; --ink: #ecece8;
  --mute: #a0a098; --line: #35352f; --accent: #f2a65a; } }
* { box-sizing: border-box; }
body { margin: 0; padding: 16px; background: var(--bg); color: var(--ink);
  font: 15px/1.45 system-ui, sans-serif; }
header { display: flex; flex-wrap: wrap; gap: 8px 16px; align-items: baseline; margin-bottom: 16px; }
h1 { font-size: 1.3rem; margin: 0; }
h2 { font-size: 1rem; margin: 0 0 8px; }
#status { color: var(--mute); }
.stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(130px, 1fr)); gap: 8px;
  margin-bottom: 16px; }
.stat { background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: 10px; }
.num { display: block; font-size: 1.6rem; font-weight: 700; }
.label { color: var(--mute); font-size: .85rem; }
main { display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap: 12px; }
section { background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: 12px;
  min-width: 0; }
p.note { color: var(--mute); font-size: .85rem; margin: 0 0 8px; }
ul { list-style: none; margin: 0; padding: 0; }
li { padding: 6px 0; border-top: 1px solid var(--line); overflow-wrap: anywhere; }
li:first-child { border-top: 0; }
q { color: var(--accent); }
.empty { color: var(--mute); margin: 0; }
`;

const BODY = `
<header><h1>Tessel dashboard: <span id="repo"></span></h1><span id="status">Connecting</span></header>
<div class="stats" id="summary"></div>
<main>
<section><h2>Active claims</h2><ul id="claims"></ul></section>
<section><h2>Denials</h2>
<p class="note">A denial is not a prevented conflict. Only the verified outcomes below count.</p>
<ul id="denials"></ul></section>
<section><h2>Denial verification</h2><ul id="verified"></ul></section>
<section><h2>Waiting</h2><ul id="waits"></ul></section>
<section><h2>Races</h2><ul id="races"></ul></section>
<section><h2>Held for review</h2><ul id="reviews"></ul></section>
<section><h2>Merges</h2><ul id="merges"></ul></section>
<section><h2>Event feed</h2><ul id="feed"></ul></section>
</main>
`;

/** The page with its inline script and style bound to `nonce` by a Content-Security-Policy. */
export function dashboardPage(nonce: string): Response {
  const html =
    `<!doctype html><html lang="en"><head><meta charset="utf-8">` +
    `<meta name="viewport" content="width=device-width, initial-scale=1">` +
    `<title>Tessel dashboard</title><style nonce="${nonce}">${STYLE}</style></head>` +
    `<body>${BODY}<script nonce="${nonce}">${PAGE_SCRIPT}</script></body></html>`;
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
