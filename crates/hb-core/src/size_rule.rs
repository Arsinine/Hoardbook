//! The v5 size rule (QURATOR-341, owner 2026-09-27): *"You can read anybody who has less data than
//! you. Everyone else you get a teaser."* ONE implementation — the asker's pre-check (should I
//! send an access request?) and the answerer's re-check (should I mint the key grant?) both call
//! [`may_read`], so the two sides can never disagree about the rule. Spec: `spec/reading.md`, v5.
//!
//! Both totals are `Teaser::total_bytes` — computed at publish, absent ⇒ 0.

/// May a reader whose total listed size is `reader_total` read an author whose total is
/// `author_total`?
///
/// - **Ties read** — `reader ≥ author`.
/// - **Zero reads nothing** — a reader with `0` gets teasers only, even of another zero-size
///   author (spec: "yours = 0 ⇒ teasers only; your first collection is what unlocks everyone at or
///   below you"). This is also what makes an ABSENT `total_bytes` (serde-default 0) fail closed.
pub fn may_read(reader_total: u64, author_total: u64) -> bool {
    reader_total > 0 && reader_total >= author_total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ties_read_and_bigger_reads() {
        // P-10 mutation: `reader_total >= author_total` → `reader_total > author_total` reds the tie.
        assert!(may_read(10, 10), "a tie reads");
        assert!(may_read(11, 10));
    }

    #[test]
    fn smaller_reader_gets_the_teaser() {
        // P-10 mutation: delete `&& reader_total >= author_total` — this reds.
        assert!(!may_read(9, 10));
    }

    #[test]
    fn zero_reads_nothing_even_another_zero() {
        // P-10 mutation: delete `reader_total > 0 &&` — the zero/zero case reds.
        assert!(!may_read(0, 0), "an absent or zero total grants nothing");
        assert!(!may_read(0, 5));
        assert!(may_read(1, 0), "a non-zero reader reads a zero-size author");
    }
}
