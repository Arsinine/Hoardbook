# QURATOR-79 — Carrier 4: peer re-serve of cached public manifests (design)

Design pass, 2026-08-26. No production code was changed. Owner ruled 2026-08-25: peers re-serve
cached metadata while the owner is offline. This document is what an implementation agent works
from; it does not re-litigate whether the big-relay carrier satisfies the intent (settled: it does
not).

Every code claim below was verified against the tree at `main` (932b23e) during this pass unless it
is explicitly marked **unverified**. Verification notes are collected at the end; one quoted
sentence from the tracker did not match the docs and is flagged there.

---

## 0. What carrier 4 actually is (the decomposition the rest builds on)

The tracker frames carrier 4 as one thing. Verified against the code, it is five, and only two of
them are new:

| Piece | Status |
|---|---|
| (i) An affordance for **C to decide** to hand out a cached envelope | **new** (nothing today can export or send a *cached* copy — `export_manifest` builds only from C's own drafts, `commands/collection.rs:892`) |
| (ii) A **transport** from C to D | **new wiring, reuses an existing plane** (see §1) |
| (iii) **D's acceptance gate** | **exists unchanged** — `accept_manifest_bytes` / `open_manifest`, `commands/browse.rs:764` / `:861` (re-verified today; drifted from `:736`/`:833`) |
| (iv) **Public-only structural enforcement** | **new** — the incumbent fence guards the producer, and carrier 4 opens a serve path that bypasses it (§3) |
| (v) **Staleness UX** | exists, needs a provenance dimension (§6) |

The most important verified fact for the whole design: **D's gate already runs at the only place
that can evaluate entitlement — on D, against A's signature and A's browse key.** C is not, and
cannot be, an authority on D's entitlement. Everything C's side adds is C's *own consent to serve*,
which is exactly what the existing ticket machinery already models.

---

## 1. The serving path

### 1.1 Transport decision (four candidates examined)

- **T1 — re-serve over the existing iroh fulfil plane.** C acts as a second-class issuer for
  envelopes it has cached: reuses `ManifestPlane`, `ManifestSource`, `TransportTicket`,
  `claim_manifest_ask`/`spend_manifest_ask`, and the redeem-on-arrival flow wholesale. Covers the
  full envelope size range the plane already handles (16 MB; pinned by
  `a_multi_megabyte_manifest_crosses_a_real_connection`, `transport.rs:1209`, drifted from `:1188`).
  **RECOMMENDED.**
- **T2 — DM the envelope directly.** Rejected: a NIP-17 gift-wrapped DM is NIP-44-bounded (~64 KB
  plaintext) while a manifest is bounded at 16 MB, so this is a partial feature for anything but
  small collections, and chunking DMs is a new split protocol (Simplicity First violation).
- **T3 — export the cached envelope to a `.hbmanifest` and hand it over out of band.** Nearly free
  (a "Share cached copy…" that writes the cached envelope JSON to a picked path; D imports it with
  the existing `import_manifest`, `npub = A`). Keep as the **fallback leg only**, mirroring how
  `send_full_list` keeps Export reachable. Its existence is worth one owner confirmation (§8).
- **T4 — Nostr re-publication by C.** C cannot re-sign A's KIND_LISTING family (and the events C
  fetched were rendered and discarded, not retained), so this means a *new event kind signed by C*
  carrying A's envelope, plus a new read-side lookup in `fetch_full_listing_if_current`. It is also
  the only option where D finds the copy *without asking C* — which is precisely the
  advertise-shaped privacy surface §5 rejects. Rejected.

T1 wins because it is the only candidate that (a) covers the full size range, (b) reuses the
reviewed authorization spine instead of inventing a parallel one, and (c) touches no INV-4′ fence —
same `ManifestPayload`, same ceiling, same files, so the CI sweep's `TRANSPORT_FILES` list and all
four probes continue to apply unchanged **provided no new iroh-using module is created** (the sweep
fails closed on an unlisted iroh user, which is the guard working).

### 1.2 The concrete flow (recommended shape)

Actors: **A** owner (offline), **C** cache-holder, **D** requester.

1. D browses A's teaser via share code; it truncates. D clicks *Ask owner* — the request-DM goes to
   A's inbox (`request_manifest`, `commands/chat.rs:931`, drifted from `:930`) and sits unanswered.
   Dead end, as today.
2. D, in chat with C (an existing contact — a stranger hits C's Q7 Request bucket, `dm_quarantine.rs`,
   and nothing is answerable until accepted), sends a **forward-request**: the same tagged
   `manifest_request` body with one new optional field —
   `{hb:"manifest_request", author_npub: A, slug, fingerprint_seen, ask_nonce}`.
   `author_npub` absent = the existing meaning (you, the recipient, own it). This is exactly how
   `ask_nonce` was added (`new_ask_nonce`/`build_manifest_request`, `chat.rs:855-921`, drifted from
   `:859-925`; wire-freeze precedent test `manifest_request_ask_nonce_is_wire_frozen` at
   `chat.rs:1169`, drifted from `:1168`).
3. C's chat recognises it (a sibling of `lib/transport-ticket.ts`'s recognisers) and shows a card:
   *"D asks for A's «slug» — you hold a cached copy from <date>*"** only when C's cache has an entry
   for `(A_npub, slug)`; otherwise the card shows the ask plainly and C answers in prose. C's human
   clicks **Send cached copy**. (M17 ruling #4 holds: nothing auto-sends.)
4. C's side — a new command `send_cached_manifest(npub, author_npub, slug)`, a thin sibling of
   `send_full_list` (`commands/fulfil.rs:90`, drifted from `:83`):
   - resolve the cache entry (`manifest_cache::get`, keyed `(npub, slug, fingerprint)`; when D's
     request carried `fingerprint_seen`, prefer the matching entry, else C's newest);
   - parse `ManifestEnvelope::from_json`, **verify `envelope.verify_author(A)` before anything
     moves** (C must not be talkable into serving B's envelope under an A-scoped request — see
   §2), then `ManifestPayload::seal(&envelope)` (the ceiling applies as everywhere);
   - `ensure_endpoint(..., Role::Listen)` — unchanged;
   - mint a ticket via `issue_ticket`, carrying the new optional `author_npub`;
   - `record_issued_ticket` (the redeem-time contact check that follows is **C's** standing for
     **D**, which is the correct semantics: C is the issuer now);
   - DM the ticket to D — unchanged.
5. D's side — `redeem_manifest_ticket` (`fulfil.rs:247`, drifted from `:223`) with one scoping
   change: the ask-ledger key and the `accept_manifest_bytes` call use `author_npub` where they
   today use the responder's npub. `claim_manifest_ask`'s durable atomic claim, `fetch_manifest`'s
   inside-the-ACK-window gate, `spend_manifest_ask` after success: all unchanged.
6. D's gate is `accept_manifest_bytes(npub = A, expected_slug = ticket.slug, raw, newest_fp =
   D's teaser fingerprint, cache_required = true)` — the **same function**, per the W4 extraction
   rule ("one path, so there is nothing to drift", `browse.rs:764`, drifted from `:736`). Author
   pinned to A, signature before decrypt, slug bound, completeness required, staleness flagged.

### 1.3 How this diverges from `seal → bind → mint → record → DM`

| Step | Under carrier 4 |
|---|---|
| **seal** | **Diverges — this is the whole feature.** Today `payload()` rebuilds from the live draft (`build_slug_manifest`, "built from the collection as it is NOW", owner ruling ② 2026-07-31). C has no collection; the envelope is **served as cached, never rebuilt**. Ruling ② therefore has *no analogue* here, and must not be stretched to demand one. |
| bind | Unchanged (`ensure_endpoint`, `Role::Listen`). |
| mint | Unchanged (`issue_ticket`), plus the optional `author_npub` field. |
| record | Unchanged (`record_issued_ticket`); the standing it feeds is C's for D. |
| DM | Unchanged. |
| redeem | Unchanged except the author scoping in the ask-ledger key and the accept call. |

### 1.4 Named touch-points

- `crates/hb-app/src/transport.rs` — `ManifestSource::payload` **widens from `fn payload(&self, slug:
  &str)` to `fn payload(&self, request_id: &str)`** (real signature verified at `transport.rs:178`;
  real call site `source.payload(&issued.ticket.slug)` at `transport.rs:314` — see §3 for why the
  slug-only shape cannot carry carrier 4 and what replaces it). `FetchRequest`,
  `serve_manifest_stream` (slug-binding check itself needs no change: A's envelope declares A's bare
  slug and the ticket still carries that slug; the *author* is pinned separately, below), `issue_ticket`.
- `crates/hb-core/src/ticket.rs` — `TransportTicket` gains
  `#[serde(default, skip_serializing_if = "Option::is_none")] pub author_npub: Option<String>`
  (`None` = issuer's own collection). Follow the `ask_nonce` precedent rather than bumping
  `TICKET_V` — **but see §8, this touches a pinned wire discriminant and needs the ruling.**
- `crates/hb-app/src/manifest_source.rs` — `StoreManifestSource` gains the cache-resolving branch;
  `contact_standing` untouched.
- `crates/hb-app/src/manifest_cache.rs` — read-only use; no schema change needed (a `CacheEntry`
  already carries `npub`/`slug`/`fingerprint`/`last_access` in plaintext, so the re-serve source
  can resolve "newest for (A, slug)" by scanning; if scanning is too slow at 256 MB, a small index
  is a follow-up, not v1).
- `crates/hb-app/src/commands/fulfil.rs` — new `send_cached_manifest`; `redeem_manifest_ticket`
  author-scoping.
- `crates/hb-app/src/commands/chat.rs` — `build_manifest_request` optional `author_npub`; request
  recogniser.
- `crates/hb-app/src/store.rs` — `record_manifest_ask` / `claim_manifest_ask` / `spend_manifest_ask`
  key widened to include the author (lenient-load pattern for pre-existing entries: an ask recorded
  without an author is `author = the asked peer`, i.e. today's semantics). **`IssuedTicketRecord`
  (`store.rs:1276`, fields verified: `ticket`, `redeemer_npub`, `consumed_at`, `delivered_bytes` —
  no cache key today) gains `served_fingerprint: Option<String>`**, set by `send_cached_manifest` at
  mint time to the fingerprint of the cache entry the human actually approved. This is the resolved
  cache key §3 depends on: serve time replays a mint-time decision instead of re-resolving "newest
  for slug".
- `crates/hb-app/src/commands/browse.rs` — `accept_manifest_bytes` unchanged (that is the point);
  caller passes A's npub.

---

## 2. Provenance / authorization — the check INV-4′'s fences do not cover

**Recommendation: yes — holding the share code is sufficient authority, and no new server-side
entitlement check should be built.** The reasoning, sharpened:

- **Content authenticity is already unforgeable by C.** The envelope is BIP-340-signed by A over
  `manifest_v‖created_at‖slug‖fingerprint‖sha256` (`hb-core/src/manifest.rs`), and D's gate pins
  the author to the browsed peer *before* the signature is checked, then the signature before any
  decrypt (`open_manifest`, `browse.rs:861`, drifted from `:833`). C passing off its own envelope as
  A's fails `verify_author`. C passing off a tampered A envelope fails the digest. Nothing to add.
- **Entitlement is the browse key, and only D can evaluate it.** The body is browse-key symmetric;
  D without A's key gets ciphertext. C cannot hand D a key D does not have — C can only hand
  ciphertext (C may itself be a *pure carrier*, unable to read what it re-serves; verified: the
  cache stores envelopes and nothing in the re-serve path needs to decrypt). So the entitlement
  check already exists, already runs on D, and is already unforgable by the serving party.
- **What C's side actually needs is C's own consent gate** — and that is precisely what reusing the
  ticket machinery gives: `contact_standing` (`manifest_source.rs:30-42`, drifted from the
  `:24-41`/`:30-44` this document cited inconsistently before — the function's real span is 30-42),
  re-read live in `issued()`, means a D that C removed after approving loses the re-serve exactly as
  a removed contact loses an owner-issued ticket today. The revocability property survives with C in
  A's chair.
- **C-side provenance fence (new, and required):** C must verify the envelope's author against the
  *requested* author before serving. Without it, a D could request `(author = A, slug = s)` and be
  served an envelope for the same slug authored by B — D's own gate would refuse it (author pin),
  so this is not a disclosure hole, but it is a serving-correctness hole and costs C a spurious
  ticket spend. One `verify_author` call in `send_cached_manifest`.

**Counter-argument — CORRECTED 2026-08-26 after adversarial review.** The argument this section
originally carried ("only *pre-re-key* ciphertext, i.e. snapshots D could have retained a copy of
anyway") was **wrong**, and understated what carrier 4 gives away. Two cases defeat it:

- **Block without re-key.** A removes or blocks D *after* approving it. Today `contact_standing`
  returns Unknown/Blocked and `authorize_redemption` refuses the redemption
  (`manifest_source.rs:30-42`, `ticket.rs:290-307`, drifted from `:257-276`). Under carrier 4, C
  serves D directly and — if
  C's cache is *current* — D receives the current full manifest **it never held**. "Could have
  retained anyway" is a possession conflation: the whole point of the feature is that D does *not*
  already hold the snapshot.
- **Leaked code.** A D that never obtained the code from A can today read teasers only; the full
  manifest is gated by A's approval on the ticket carrier. Carrier 4 hands any leaked-code holder
  the full listing whenever any contact holds it.

**The argument that does work — which this design originally failed to use:** the **big-relay
carrier already grants approval-free full manifests to any browse-key holder.**
`fetch_full_listing_from(peer, slug, browse_key, …)` (`hb-net/src/browse.rs:485`) is a pure
key-gated relay read with **zero** approval-gate references in its body (verified 2026-08-26). So
"the browse key IS full-manifest authority" is not a concession carrier 4 invents — it is the
**incumbent M16 entitlement model**, and carrier 4 extends it from the relay carrier to the cache
carrier. The ticket carrier's redeem-time veto is the *outlier*, not the norm.

**What is genuinely lost:** A's redeem-time veto over cached copies, permanently — *including the
blocked-contact case above*. That is a real reduction in A's control and must be ruled on at that
scope (§8 #2), not as "pre-re-key ciphertext".

**Confirmation-oracle residual:** an answer-only C that replies "I hold that" to anyone who asks is
a small enumeration surface ("which of A's collections has C browsed"). It is bounded by C's own
contact gate + the Q7 stranger quarantine + the human click, i.e. C's human is the privacy boundary,
consistent with every other consent surface in the app.

---

## 3. Structural public-only enforcement

**The gap, sharpened:** `build_slug_manifest` refuses `Visibility::Private`
(`commands/collection.rs:830`, the check itself at `:841`; red test
`build_slug_manifest_refuses_a_private_collection` now at `:2861` — line moved from the `:2507` this
document originally cited, drifted by QURATOR-161's 638-line `collection.rs` growth on 2026-08-30;
re-verified against HEAD today). That fence guards the **producer**. Carrier 4's serve path reads the
**cache**, not the producer, so the incumbent fence does not gate it. This is the concrete instance of
the tracker's "enforced structurally rather than by comment".

Verified foundation: a private collection **has no `ManifestEnvelope` representation at all** — it
seals via `priv_listing`'s per-recipient CEK wrap over nostr seal/gift-wrap events
(`hb-core/src/priv_listing.rs:1-22`), never truncates, never enters `parse_manifest_source`, and the
manifest cache stores only envelopes. Re-counted today, by hand, against HEAD (not yet by a CI
sweep — that gap is fix (b) below): `manifest_cache::put` appears **3 times** in `crates/hb-app/src`
— **exactly 1 production call site**, `commands/browse.rs:799` inside `accept_manifest_bytes`, and 2
test-only sites (`commands/browse.rs:1246`, inside `#[test] fn
resolve_from_cache_upgrades_a_truncated_teaser_and_gates_on_the_signed_fingerprint`; `store.rs:2002`,
inside `mod tests`) — both after their file's `#[cfg(test)]` boundary (`browse.rs:994`,
`store.rs:1426`). So the type is the first fence — but per INV-4′'s own logic and CLAUDE.md §9
("any one mechanism alone is just a comment"; "assume any privacy guard is decorative until a
mutation of production code reds it"), it needs company. Two things changed here from the prior pass
of this document, both forced by adversarial review:

**(a) The load-bearing fence's real shape.** The previous draft of this section specified a re-serve
`payload` function "keyed on `(author_npub, slug, fingerprint)`". That does not match the seam: the
actual trait method is `fn payload(&self, slug: &str) -> Result<ManifestPayload>`
(`transport.rs:178`), called as `source.payload(&issued.ticket.slug)` (`transport.rs:314`) —
**slug only**, and `TransportTicket`'s fields are exactly `hb`, `ticket_v`, `request_id`, `slug`,
`node_addr`, `issued_at`, `ask_nonce` (`hb-core/src/ticket.rs:98-123`, verified against HEAD) — no
author, no fingerprint. Resolving "newest cached envelope for this bare slug" at serve time is a real
bug, not a simplification: slugs are bare names, so C holding both B's «roms» and A's «roms» would
serve **B's envelope under an A-scoped ask**. D's gate refuses it on the author pin — but only *after*
`authorize_redemption` has already granted the ticket and `into_consumed` has run, which is exactly
the spurious-spend the mint-time fence in §2 exists to prevent (that fence runs at mint, not serve).

Two routes were open: widen the shared `ManifestSource` trait to carry more than a bare slug (cost:
touches the owner-issuer path too, the drift risk the one-seam rule exists for), or accept "newest for
slug" and its cross-tenant collision. **This design takes the widening route**, in its narrowest
form: `ManifestSource::payload` changes from `fn payload(&self, slug: &str)` to `fn payload(&self,
request_id: &str)`. `request_id` is not a new concept at this seam — `IssuedTicketRecord`s are
already stored keyed by exactly that string (`store.rs`'s `load_issued_tickets: HashMap<String,
IssuedTicketRecord>`, verified), and `issued()` was just called with the same `request_id` one line
above the `payload()` call in `serve_manifest_stream`. Each implementation resolves its own payload
from it:
- **Owner path** (`StoreManifestSource`, unchanged behavior): looks up its own issued-ticket record
  for `request_id` to recover `slug`, then `build_slug_manifest(slug)` exactly as today — ruling ②
  ("built from the collection as it is NOW") is untouched, because C's cache-serving branch is a
  different code path in the same struct, not a change to this one.
- **Cache-serving branch** (new, in the same `StoreManifestSource`): looks up the *same* issued-ticket
  record, reads `ticket.author_npub` (Some when this is a re-serve) and the new
  `IssuedTicketRecord.served_fingerprint` (§1.4, `store.rs`) — the exact `(author_npub, slug,
  fingerprint)` triple the human approved when `send_cached_manifest` minted this ticket — and fetches
  that exact `manifest_cache` entry. **Resolution happens once, at mint, under the human's "Send
  cached copy" click; serve time replays it rather than re-guessing "newest for slug".** This is the
  fix `IssuedTicketRecord` needed and did not have (§1.4 adds `served_fingerprint`).

This keeps the load-bearing property `payload` always had — the parameter is an opaque identifier
(a `request_id` string is no more of a byte-slice-or-path escape hatch than a `slug` string was), it
still returns only a `ManifestPayload` obtainable via `ManifestEnvelope::from_json` →
`verify_integrity` → `verify_author` → `ManifestPayload::seal`, and it still never accepts a
`Vec<u8>`, a path, an event, or a fallback source. A private listing cannot physically enter the path
because no value of `request_id`'s type can name one — the fence in the numbered list below still
holds, restated against the real signature:

1. **Type/seam fence — LOAD-BEARING.** `ManifestSource::payload(&self, request_id: &str) ->
   Result<ManifestPayload>` (widened as above). No `Vec<u8>` parameter, no path, no event, no
   fallback source; a `ManifestPayload` is obtainable solely via `ManifestEnvelope::from_json` →
   `verify_integrity` → `verify_author` → `ManifestPayload::seal`. This mirrors the seam's existing
   documented discipline ("no path parameter and no byte-slice parameter… mechanism 1 reaching one
   layer up from the type into the seam", `transport.rs:170-172`) and, one layer further,
   `ManifestPayload`'s constructor discipline.
2. **Compile-shape pin (red-testable).** A `compile_fail` doctest in the re-serve module, same
   trick as `RedemptionGrant::into_consumed` (`ticket.rs`): calling the serve function with raw
   bytes / an event / a draft does not compile. The mutation this reds on: a later edit widening
   the signature "just to also support X".
3. **Runtime red tests.** (a) A `request_id` whose `IssuedTicketRecord` has no `served_fingerprint`
   (owner-issued, not a re-serve) never falls through to the cache; a `request_id` whose
   `served_fingerprint` names a cache-miss **errors — never falls through** to any other source
   (reds on a later "helpful" fallback to `build_slug_manifest`, which would silently serve C's own
   same-slug collection as if it were A's). (b) An envelope that fails `verify_author(requested
   author)` is refused before `seal`, at mint time (§2's fence) — proving the *stored*
   `served_fingerprint` can only ever point at an author-checked entry. (c) **Stale non-clobber**
   (§6): a re-served older fingerprint must not shadow a newer teaser.
4. **CI sweep — defence in depth, two probes.**
   - **(existing shape) Re-serve-surface probe.** Extend the INV-4′-style sweep in
     `.github/workflows/ci.yml:79` (confirmed unmoved — "INV-4′ sweep — the plane carries manifests,
     never collection files" is still the step name at that line) with a re-serve-surface probe: the
     re-serve module (and `manifest_cache.rs`) must not reference `priv_listing`,
     `seal_private_listing`, `private_audience`, or `browse_private`; sweep at the source with
     comment-stripping (the sweep already implements `code_only`) and per-file counts, absolute
     paths, `command -v` the tool — per the P-5 / `rg -E` traps in CLAUDE.md.
   - **(b) NEW — `manifest_cache::put` single-writer allowlist.** This is fix (b) this pass adds:
     "exactly one production writer" was, until today, hand-verified prose, not a CI gate — a later
     feature writing private data into the cache would break the fence silently, because the existing
     sweep greps for `priv_listing`, not for new `put` call sites. Per CLAUDE.md §9 ("a hand-written
     list of sites cannot fix a hand-written list of sites — invert to an allowlist with per-file
     counts"), the new step must: grep `crates/hb-app/src` for both the qualified form
     (`manifest_cache::put\(`, for external callers) and the bare form (`\bput\(` within
     `manifest_cache.rs` itself, since a same-module caller does not need the `manifest_cache::`
     prefix — verified today that this bare form is what the module's own tests already use, so the
     probe must not miss it); restrict each file's scan to the region **before** its first
     `#[cfg(test)]` line (verified boundaries: `browse.rs:994`, `store.rs:1426`,
     `manifest_cache.rs:142`) so test fixtures don't count as writers; comment-strip with the
     existing `code_only`; print one line per file scanned — `<absolute path>: <count>` — so "0 hits"
     reads as "0 hits across N files scanned", never as "did the sweep run at all"; and fail unless
     the non-test hits are **exactly one**, at **`crates/hb-app/src/commands/browse.rs`**. Any second
     production site, or a site outside `commands/browse.rs`, fails the build. This is the mechanism
     that makes today's hand count (3 total, 1 production, both above) durable instead of a one-time
     observation.

Which is load-bearing: **#1**. #2–#4 are decoration until each is shown red by a mutation of
production code (P-10); the implementation ticket must include the mutation runs, not just the
green suite.

---

## 4. Revocation

**Recommendation: no change — "other people's caches go stale eventually" is acceptable, and re-key
does not need to become a real revocation.** The analysis:

- A re-key (SEMANTICS.md Q5, ruled 2026-07-03: all-or-nothing, silent) changes the key that gates
  **future** publication. Everything published after the re-key is unreadable to the cut-off holder.
- Everything published *before* was already in holders' hands — a holder could always have kept a
  decrypted copy. Carrier 4 makes old *ciphertext* easier to obtain, but the old key the holder
  needs to read it is one they already had. **No new reading power is created.**
- Therefore the AB9 property ("the cache is not killed by a re-key") extends to other people's
  caches without changing what a re-key protects. The blast radius of the *documented property*
  widens from "the recipient's own cache" to "any cache-holder's cache" — the semantics' text does
  not change, but its scope does.

**Marked as needing an owner ruling** (§8 #2) because it touches a documented invariant's scope
even though no rule text changes. Two adjacent notes for the record: A has no lever at all over
C's serving (inherent to the ruling); and any future "purge my collections from peers" feature
would be new work under INV-8 (deliberate deletion) — explicitly **out of scope** here, per
Simplicity First.

---

## 5. Advertise vs answer-only

**Recommendation: answer-only, human-mediated.** C never publishes what it holds. Discovery is: D
asks C — in chat, by name, the way a human asks a mutual — and C's client turns a recognised
forward-request into a card only when C actually holds a matching entry (and reveals even that
much only to a contact, behind Q7 for strangers and the human click for everyone).

- **Under answer-only**, the discovery mechanism is social: D must know to ask C. The affordance is
  the forward-request card; the minimum viable version is the tagged request body plus the card,
  with no directory, no holdings event, nothing queryable.
- **Under advertise**, C would publish an event enumerating `(A_npub, slug)` pairs it holds. That
  is a public record of **C's browsing history** — a who-browsed-whom oracle for anyone, forever,
  on relays — and it requires a new event kind (new wire surface on the read side too). Rejected.
  It is also unnecessary to the owner's sentence: "someone else could pass on my collection
  metadata" describes answering, not broadcasting.

The card's "you hold a cached copy" line is a *private, per-conversation* answer and is the minimum
reveal the affordance needs to exist; it is not advertising in the publish sense.

---

## 6. Staleness UX

Existing treatment, verified: `ImportedManifest.stale` (`browse.rs:683`, drifted from `:655`) →
toast *"Imported an older version of this list. Ask the owner for a fresh manifest."* at
`ui/src/routes/browse/+page.svelte:238` (drifted from `:221`; exact current copy re-verified above —
period, not em dash, between the two sentences); the owner-side note in
`ui/src/lib/components/ManifestFulfilCard.svelte:64` (re-verified, unmoved); the field documented at
`ui/src/lib/types.ts:93` (re-verified, unmoved). Under carrier 4 the message is wrong in two ways:
the owner cannot be asked (offline), and the user cannot tell **who served this or how old it is**.

Changes:

- `ImportedManifest` (Rust `browse.rs:683`, drifted from `:655`; TS mirror `ui/src/lib/types.ts`) gains
  `served_by: Option<String>` (C's npub) and `cached_at: Option<u64>`. `collection.manifest_imported_at`
  already carries the envelope's own `created_at` (set in `open_manifest`) — surface it rather than
  inventing a second clock.
- `ui/src/routes/browse/+page.svelte` — the stale branch toast becomes provenance-aware: stale +
  re-served ⇒ *"Older copy served by <C> from <date> — <A> is offline; ask again when they're
  back."* The non-stale re-serve case gets a lighter note ("served by <C>'s cached copy").
- `ui/src/routes/chat/+page.svelte` + a new `ui/src/lib/components/ManifestForwardCard.svelte` (or a
  `kind: 'forward'` variant of `ManifestFulfilCard`) — C's side of the card: shows D's ask, the
  author, the cached date, and **Send cached copy** / Decline. The card must render the
  "you don't hold this" state without an affordance.
- `ui/src/lib/transport-ticket.ts` — `ticketAnswersOurAsk` extended to the (responder, author,
  slug, nonce) identity so a re-serve ticket is redeemed on arrival exactly like an owner ticket.

**A property worth pinning (verified, needs a test):** the cache is keyed
`(npub, slug, fingerprint)` and `resolve_from_cache` (`browse.rs:825`, drifted from `:797`) gates on
the *teaser's* fingerprint (the refusal check itself at `browse.rs:844`: `if
envelope.snapshot_fingerprint != fingerprint`), so an older re-serve lands *beside*, never *over*, a
newer teaser — the existing keying already prevents stale-clobber.
Write the discriminator test in the same commit (P-13: attribute-pinning suites are blind to shape).

Gates for the UI work: vitest with `--pool=forks --no-file-parallelism`, the `svelteTesting()`
plugin, mount the real page rather than source-scan (P-4), and `npx svelte-check --threshold error`
from `crates/hb-app/ui` for any page-level change (P-8).

---

## 7. Test plan sketch

Per CLAUDE.md §5 this is a network change: **both** regression unit tests **and** integration
tests, or it is not shippable. Structural fact that shapes everything (verified in
`crates/hb-it/Cargo.toml`): **`hb-it` links `hb-core` and `hb-net` only — it cannot link `hb-app`.**
The re-serve logic lives in hb-app (cache, source, commands, UI), so:

- **CI's `cargo test --workspace` leg does compile and run hb-app *unit* tests** — the regression
  half can be discharged in CI. What CI structurally cannot discharge is the *integration* half for
  hb-app-side behaviour: the CI L2 leg exercises hb-core/hb-net against an ephemeral relay and
  never links the app.
- **The integration half lives in `hb-wan-it`** (in-crate bin, links hb-app, manual pre-release
  gate — the WAN-M precedent of driving `transport::fetch_manifest` with the real
  `accept_manifest_bytes` gate inside, `wan_it/suite_wan_m.rs`). Prefer iroh-direct rows over
  DM-propagation rows where possible, given QURATOR-125's VPS-relay nondeterminism.

| Piece | Suite | Notes |
|---|---|---|
| `TransportTicket.author_npub` wire freeze | hb-core unit (`wire_freeze.rs` pattern) | present-field, absent-field, and wrong-discriminator cases; mirrors the `ask_nonce` freeze test at `chat.rs:1169` (drifted from `:1168`) |
| request-body `author_npub` freeze | hb-app unit (`commands/chat.rs` tests) | same three cases |
| re-serve source: cache-miss errors, no fallthrough, author-pin refusal, `verify_author` before `seal` | hb-app unit | each with its named mutation run (P-10) |
| compile-shape pin (raw bytes / event / draft don't compile) | hb-app `compile_fail` doctest | the `into_consumed` trick |
| stale non-clobber (older re-serve does not shadow a newer teaser) | hb-app unit, `resolve_from_cache`-level | the P-13 discriminator |
| ask-ledger keying incl. lenient load of pre-author entries | hb-app unit (`store.rs`) | |
| private-only sweep | CI (`ci.yml`, extended probe) | comment-stripped, per-file counts, absolute paths |
| cross-NAT re-serve redemption with the owner's endpoint **never bound** | `hb-wan-it` WAN-M new rows (M-re1…) | owner-offline is the point: A's endpoint must not exist in the harness |
| re-serve of a mis-attributed envelope refused by D's gate over a real link | `hb-wan-it` WAN-M | |
| staleness row over a real link (synthetic mismatched fingerprint, the E2 trick) | `hb-wan-it` | mirror `suite_wan_e2e.rs` E2 |
| forward-card recognition, provenance toast, no-affordance-when-not-held | UI vitest, mounted pages | + `svelte-check` |

---

## 8. Explicit owner-ruling list (before implementation starts)

1. **Answer-only confirmed** — caps the feature to "D must know to ask C"; the alternative
   (advertise) is rejected on privacy, not cost.
2. **AB9/Q5 scope extension — RESTATED 2026-08-26 at TRUE scope.** Not merely "other people's
   caches hold pre-re-key ciphertext". The accurate concession: **carrier 4 permanently removes A's
   redeem-time veto over any cached copy.** Concretely — A can no longer stop a **blocked or
   removed contact** from obtaining A's *current* full manifest through a mutual contact, and a
   **leaked share code** yields the full listing rather than teasers only. Precedent for accepting:
   the big-relay carrier already works exactly this way (§2). Recommendation: accept — but accept
   *this*, not the narrower statement this list previously carried.
3. **Owner ruling ③ (2026-07-31) scope** — a node that has never published its own collection
   becomes a *listener* on the manifest plane when it re-serves. The ruling's rationale ("a
   redeemer should not listen merely because it redeemed" — the probeable liveness oracle,
   `bind_client_endpoint`'s doc comment, `transport.rs:724-740`, drifted from `:720-737`) is
   respected: C listens because C *serves*, and C's node id reaches
   others only inside C's own tickets, the same containment A has. Confirm this reading.
4. **Wire change route** — optional `author_npub` on `TransportTicket` and the request body via the
   `ask_nonce` serde-default precedent (no `TICKET_V` bump), vs a discriminant bump. CLAUDE.md says
   changing a wire format means bumping its discriminant *and* updating the pinning test; the
   precedent says an added optional field with `skip_serializing_if` was done without one. Pick one,
   explicitly.
5. **Non-contact requesters** — keep the Q7 stranger-quarantine + human-accept gate as the only
   path for a non-contact D (recommended), or additionally hard-refuse re-serves to non-contacts.
6. **T3 fallback** — keep or cut the "export cached copy to file" affordance alongside the in-app
   re-serve (recommended: keep; it is ~free and mirrors Export's role for `send_full_list`).

Not needing a ruling (recorded so nobody re-opens them): INV-2 (browse-key never travels — it is
something D already holds); INV-4′ (payload shape fences all still apply; only provenance is new,
addressed in §2/§3); INV-5, INV-8 (no new deletion semantics).

---

## 9. Verification notes — what was and wasn't confirmed

**2026-08-30 pass (this revision):** all six owner rulings resolved; two adversarial-review findings
fixed (§3's fence mechanism corrected to match the real `ManifestSource::payload` seam, and a
concrete CI allowlist specified for the `manifest_cache::put` single-writer claim); every `file:line`
citation in this document was re-checked against HEAD (`feffec8`) with `grep -n`, individually listed
inline at each citation rather than re-listed here. Net result: `collection.rs:830` and `:892` were
unchanged (QURATOR-161's 638 new lines in that file landed *after* both cited functions); every other
cited line in `browse.rs`, `chat.rs`, `fulfil.rs`, `transport.rs`, `ticket.rs`, and
`manifest_source.rs` had drifted from unrelated commits between 2026-08-26 (when this document was
first written) and today, by amounts from 1 to 354 lines — all corrected in place, each flagged with
its old value. `ci.yml:79`, `types.ts:93`, `ManifestFulfilCard.svelte:64`, `hb-net/src/browse.rs:485`,
and `hb-core/src/priv_listing.rs:1-22` were re-checked and found unmoved. One citation was not just
stale but **wrong on the content, not only the line**: the browse-page stale toast's copy is
"Imported an older version of this list. Ask the owner for a fresh manifest." (period, sentence
case) — this document had it as an em-dash-joined single sentence; corrected in §6. `manifest_cache::put`
was re-counted from source today (not re-run through any CI mechanism, because none exists yet — that
absence is exactly FIX 2 in §3): 3 total occurrences, 1 production (`browse.rs:799`), 2 test-only
(`browse.rs:1246`, `store.rs:2002`), both confirmed past their file's `#[cfg(test)]` boundary.

**Verified against the code (all claims above rest on these):**

- `manifest.rs` head doc: "Browse-key symmetric, not per-recipient (M16 decision): access = holding
  the full `hbk1…` share code" — present, in the module doc block around lines 22-28.
- `priv_listing.rs:1-22`: per-recipient CEK wrap; private collections never truncate, never produce
  an envelope.
- `manifest_source.rs:30-42` (drifted from `:24-41`): `contact_standing` (block → decline →
  contact-hood; `Unknown` refused), re-read live in `issued()`.
- `transport.rs`: `FetchRequest`, `ManifestSource` seam, `serve_manifest_stream` gate order
  (issued → ticket equality → `authorize_redemption` → payload → slug binding → send → ACK →
  consume), `ManifestPlane` in-flight/poisoned sets, `fetch_manifest` with the accept-gate inside
  the ACK window, `issue_ticket`, `bind_client_endpoint`'s no-ALPN dial-only design and its ruling
  ③ rationale.
- `ticket.rs`: field list, no expiry, `ask_nonce` serde-default precedent, `authorize_redemption`,
  the `into_consumed` compile-fail pattern.
- `fulfil.rs`: both commands end to end, the seal→bind→mint→record→DM ordering notes, the durable
  atomic `claim_manifest_ask` gate, `spend_manifest_ask` after success.
- `browse.rs`: `accept_manifest_bytes` (contact → share code → author pin → slug bind →
  completeness → cache write, `cache_required` semantics), `open_manifest` (stale =
  `!matches_fingerprint`, surfaced never blocking), `resolve_from_cache` fingerprint gate.
- `manifest_cache.rs`: `(npub, slug, fingerprint)` keying, LRU, `CacheEntry` carries its key
  fields in plaintext. Re-counted today by hand (not yet by CI — §3 FIX 2): `manifest_cache::put`
  has exactly one production call site (`browse.rs:799`), `get` one (`resolve_from_cache`,
  `browse.rs:825`); the other `put` hits are test-only, in `browse.rs:1246` and `store.rs:2002`.
- `collection.rs:830` + `:2861` (test line drifted from `:2507` — the only citation this document
  had into `collection.rs` whose line number QURATOR-161 actually moved; the function-definition
  citations `:830` and `:892` were unaffected): `build_slug_manifest` refuses Private, with its red
  test `build_slug_manifest_refuses_a_private_collection`.
- `hb-it/Cargo.toml`: depends on hb-core, hb-net, nostr-sdk, nostr, tokio, anyhow, serde_json,
  chrono, tracing-subscriber — **no hb-app**. `hb-wan-it` is a bin inside hb-app.
- `ci.yml:79+`: the INV-4′ sweep, `TRANSPORT_FILES`, the fails-closed iroh-user scan, and
  `cargo test --workspace` (so hb-app unit tests do run in CI).
- QURATOR-79's Plane row (retrieved this session) matches the task statement verbatim.

**Not verified / flagged:**

- **The tracker's quoted sentence "a re-key kills NEW listings, not the cache" was not found
  verbatim** in `SEMANTICS.md`, `INVARIANT_AUDIT.md`, or `docs/` — those hold the Q5 ruling
  ("all-or-nothing, silent; every share code ever handed out stops decrypting everything;
  cut-off holders see listings go locked"), which carries the same substance. Treat the tracker's
  phrasing as a gloss; §4 argues from the Q5 text, not the gloss.
- Whether any *second* CI leg runs the ephemeral-relay L2 was not re-confirmed this session (the
  task statement says it exists and is green; `ci.yml`'s workspace-test leg was confirmed). The
  design depends only on the structural fact, which the Cargo.toml settles.
- Performance of scanning the cache directory for "newest entry for (A, slug)" at 256 MB / many
  entries was not measured. If it matters, an index file is a follow-up; v1 should scan (the dir
  is bounded and local).
