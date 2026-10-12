# REVIEW-CARD render check (2026-10-11 00:40 EDT)

Steward deployed by Felix as version `5a37c8e6` from the deploy worktree at `ab74e1f` (steward code
identical to the review-card merge `7f5330e`). The live page needs a Cloudflare Access session, and
the session's Chrome extension was not connected, so the orchestrator rendered the deployed
`reviewPage` code locally with one realistic card (two hold reasons, a waiter, an assumer, three
touched scopes, two evidence lines) through a throwaway vitest file, and screenshotted
`review.html` inside a 390 px iframe with headless Chrome (`review-390px.png`, two frames: an
844 px viewport and the full page).

Seen at 390 px: no horizontal overflow; every section wraps (WHY YOU, WAITING · EXPOSED, INTENT
quoted as untrusted text, task ref, evidence, pushed-to-fork, WHAT CHANGED with "Load the diff",
RECEIPT with its three waits); the decision bar is sticky at the bottom of the 844 px viewport
with the note field, a greyed Approve and "Reject with a note"; buttons are 48 px tall. After the
page script ran (`--dump-dom`, 3 s virtual time, no diff endpoint reachable) the approve button
was still `<button data-action="approve" disabled="">`.

Not checked: the live page itself, the diff load, the decision round trip and the receipt polling
against the deployed steward. A direct headless screenshot at `--window-size=390` clipped the right
edge because headless Chrome clamps its window width on macOS; the iframe render is the valid one.
