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
# Known evasions, filed as follow-up, NOT fixed here: a multi-line inline table, quoted keys
# containing dots, `{ "package" = "iroh" }`, and whitespace-padded dotted headers.

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
FNR == 1 { sect = ""; cur = "" }
{
  line = strip_comment($0)
  gsub(SQ, "\"", line)
  if (line ~ /^[[]/) {
    sect = line; cur = ""
    if (match(sect, /dependencies[.]"?[A-Za-z0-9_-]+"?[]]/)) {
      cur = substr(sect, RSTART + 13, RLENGTH - 14)
      gsub(/"/, "", cur)
    }
    next
  }
  if (sect !~ /dependencies/) next
  if (line ~ /^[ \t]*#/) next
  if (cur != "") {
    if (line ~ /^[ \t]*package[ \t]*=[ \t]*"iroh"/) printf "%s:%s\n", FILENAME, cur
    next
  }
  if (match(line, /^[ \t]*"?[A-Za-z0-9_-]+"?/)) {
    key = substr(line, RSTART, RLENGTH)
    rest = substr(line, RSTART + RLENGTH)
    gsub(/"/, "", key)
    if (key == "iroh" || rest ~ /package[ \t]*=[ \t]*"iroh"/) printf "%s:%s\n", FILENAME, key
    next
  }
}
