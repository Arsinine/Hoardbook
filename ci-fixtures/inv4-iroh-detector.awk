# INV-4' manifest-side iroh detector (QURATOR-281).
#
# SINGLE-SOURCED DELIBERATELY. This program was duplicated byte-identically between the detector
# step and its self-test in ci.yml. That is P-6's shape: a guard re-emitting its own copy of the
# thing it checks is asserting against a lookalike it controls. The two copies could drift — a
# regression in the production copy would leave the self-test passing on its own healthy copy,
# with CI green and the INV-4' fence silently degraded, because the real tree's only declaration
# is the allowlisted `iroh` and so the loud-failure guards never fire.
#
# Both ci.yml steps now `awk -f` THIS file, so the self-test exercises the production bytes and
# drift is impossible by construction.
#
# Prints "manifest:localname" for every dependency whose package resolves to iroh, in every valid
# spelling: the key `iroh` itself (string, inline table, dotted, [dependencies.iroh]) bare or
# QUOTED, any renamed key carrying package = "iroh" or package = 'iroh' (TOML literal string), and
# dotted sections whose header key is bare or quoted. Trailing # comments are stripped before
# matching — a # inside a quoted string is NOT a comment start — so prose naming iroh cannot feed
# the detector. Section-aware: only *dependencies tables count.
#
# QURATOR-287 closed the four exotic spellings the line-oriented pass above still missed:
#   (1) a MULTI-LINE inline table — `quic = {` newline `package = "iroh"` newline `}` — via a
#       brace-depth state machine: a dependencies key whose inline table does not close on its
#       own line becomes `pending`, and every line inside it is tested for package = "iroh"
#       until the running depth returns to zero. Cargo's parser accepts this TODAY (verified:
#       it parses the table and only objects to the missing version), so this is a live evasion,
#       not TOML-1.1 futurology. Known bound: braces are counted on the raw line, so a literal
#       { or } inside a quoted VALUE can hold the state open across lines — the failure mode is
#       the tree's known `iroh` declaration going unprinted, which trips the detector step's
#       loud-failure assertion. ⚠ CORRECTED by the 2026-09-16 review: that "never a silent pass"
#       claim was FALSE. A brace inside a quoted value wedged the state open and swallowed every
#       later rename in the same table — and when the wedge starts AFTER the known `iroh` line,
#       the loud control still passes, so the miss is silent. Braces are now counted on
#       quote-stripped text (`strip_quoted` below) and the fixture pins the case.
#   (2) a QUOTED KEY CONTAINING A DOT — [dependencies."quic.name"] — by taking everything after
#       the `dependencies.` prefix as the section's local name instead of a fixed char class;
#   (3) a QUOTED INNER KEY — { "package" = "iroh" } — by allowing optional quotes around
#       `package` in both the inline and the dotted-section matchers;
#   (4) a WHITESPACE-PADDED DOTTED HEADER — [ dependencies . quic ] — by stripping the brackets
#       and their padding before the prefix match, so segments need not be contiguous.

BEGIN { SQ = "\047" }
# Strip a trailing comment: cut at the first # that sits OUTSIDE any quoted
# string — a # inside double quotes or an SQ-quoted literal is content.
function strip_comment(l,  i, c, q) {
  q = ""
  for (i = 1; i <= length(l); i++) {
    c = substr(l, i, 1)
    if (q == "") {
      if (c == "\"" || c == SQ) q = c
      else if (c == "#") return substr(l, 1, i - 1)
    } else {
      if (q == "\"" && c == "\\") i++
      else if (c == q) q = ""
    }
  }
  return l
}
# Blank out the CONTENTS of quoted spans so structural counting (braces) cannot be fooled by a
# `{` or `}` inside a string value — `features = ["{"]`, `branch = "dev{"`. Same discipline
# strip_comment applies to `#`. QURATOR-287 review finding 2: without this, a quoted brace wedged
# `pending` open and every later rename in that table was SILENTLY missed — a false NEGATIVE, and
# the loud known-declaration control does NOT catch it when the wedge starts after that line.
# Quotes themselves are kept, so key/value matching is unaffected; only the innards are erased.
function strip_quoted(l,  out, i, c, q) {
  out = ""; q = ""
  for (i = 1; i <= length(l); i++) {
    c = substr(l, i, 1)
    if (q == "") {
      out = out c
      if (c == "\"" || c == SQ) q = c
    } else {
      if (q == "\"" && c == "\\") { i++; continue }
      if (c == q) { out = out c; q = "" }
    }
  }
  return out
}

FNR == 1 { sect = ""; cur = ""; pending = ""; depth = 0 }
{
  line = strip_comment($0)
  gsub(SQ, "\"", line)
  if (line ~ /^[[]/) {
    sect = line; cur = ""; pending = ""; depth = 0
    hdr = line
    sub(/^[[]+[ \t]*/, "", hdr)
    sub(/[ \t]*[]]+[ \t]*$/, "", hdr)
    # The local name is everything after `dependencies.` — bare or QUOTED, dots and padding
    # and all (QURATOR-287 (2) and (4)). UNANCHORED on purpose, so a target section such as
    # [target.'cfg(unix)'.dev-dependencies.NAME] resolves its NAME exactly as it did before.
    if (match(hdr, /dependencies[ \t]*[.][ \t]*/)) {
      cur = substr(hdr, RSTART + RLENGTH)
      gsub(/"/, "", cur)
    }
    next
  }
  if (sect !~ /dependencies/) next
  if (line ~ /^[ \t]*#/) next
  if (cur != "") {
    if (line ~ /^[ \t]*"?package"?[ \t]*=[ \t]*"iroh"/) printf "%s:%s\n", FILENAME, cur
    next
  }
  # QURATOR-287 (1): continuation lines of a multi-line inline table. Test for the package
  # rename first, THEN retire the depth, so a line that both closes the table and names the
  # package (`package = "iroh" }`) still counts.
  if (pending != "") {
    if (line ~ /^[ \t]*"?package"?[ \t]*=[ \t]*"iroh"/) printf "%s:%s\n", FILENAME, pending
    structural = strip_quoted(line)
    depth += gsub(/[{]/, "&", structural) - gsub(/[}]/, "&", structural)
    if (depth <= 0) { pending = ""; depth = 0 }
    next
  }
  if (match(line, /^[ \t]*"?[A-Za-z0-9_-]+"?/)) {
    key = substr(line, RSTART, RLENGTH)
    rest = substr(line, RSTART + RLENGTH)
    gsub(/"/, "", key)
    if (key == "iroh" || rest ~ /"?package"?[ \t]*=[ \t]*"iroh"/) printf "%s:%s\n", FILENAME, key
    structural = strip_quoted(rest)
    no = gsub(/[{]/, "&", structural); nc = gsub(/[}]/, "&", structural)
    if (no > nc) { pending = key; depth = no - nc }
    next
  }
}
