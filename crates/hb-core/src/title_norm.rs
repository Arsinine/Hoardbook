//! The ONE title normaliser (QURATOR-344 slice C; the P1 foundation).
//!
//! `spec/phonebook.md` lines 12-35: titles are compared through one normaliser with
//! golden vectors, so the index, holder counts and similarity can never disagree about
//! whether two names are the same title. P3's title index, holder counts and the
//! similarity score must all consume *this* function — none pre-normalises on its own.
//!
//! Pipeline, in order (each rule carries its reason):
//!
//! 1. **NFKC + lowercase** — a full-width filename (`ＤＵＮＥ　１９８４`) and an ASCII
//!    one (`Dune 1984`) are the same key; case is never identity.
//! 2. **Trailing media extension** — stripped only when it exactly matches a known
//!    media/container/console-image extension. Known-list-only so dot-words in real
//!    titles survive: "Dr. Strangelove.mkv" keeps "Strangelove" because `.strangelove`
//!    is not on the list.
//! 3. **Bracket groups** — `[...]` groups are always release noise (scene tags, format
//!    tags, checksums) and are dropped whole. `(...)` groups are dropped UNLESS the
//!    inner content is exactly a 4-digit year in 1900..=2099, which is KEPT as part of
//!    the key: "Dune (1984)" and "Dune (2021)" are different works. An unclosed
//!    bracket keeps the rest of the string verbatim (deterministic over junk).
//! 4. **Separators** — runs of `.` `_` `-` and whitespace collapse to one space and
//!    trim, so `A.B_C-D` and `A B C D` are one key.
//! 5. **Scene tokens** — the key TRUNCATES at the first release token (resolution,
//!    codec, source, audio, "WEB DL" bigram). Everything after the first hit is release
//!    noise — including the release-group name, without enumerating group names. A name
//!    that is *only* release tokens (e.g. "FLAC", "1080p") stays itself: truncating
//!    would erase it, and a key must never be empty when the input wasn't.
//! 6. **Leading article** — "The"/"An"/"A" at the head of the key is dropped ("The
//!    Matrix" == "Matrix" folders are the same work). "A" is only dropped when the
//!    next token is longer than one character, so the initialism "A.I." keeps its "A".
//!    Mid-title articles stay: identity lives in the head of the key.
//!
//! Two decisions the golden vectors pin, spelled out:
//!
//! - **A year-less name never matches a year-ful one.** The year is part of the key
//!   when present; dropping it would merge "Dune (1984)" onto a year-less "Dune"
//!   folder of unknown provenance. Same-work/variant-year pairs are P3 similarity's
//!   job, not the key's.
//! - **Episodes keep their SxxEyy.** An episode is a unit of a work, not a variant of
//!   it: "Show S01E02" and "Show S01E03" must never merge, and neither may merge with
//!   the whole-series "Show".

use unicode_normalization::UnicodeNormalization;

/// Trailing extensions treated as file/container noise (exact match, after lowercasing).
/// Media, audio, ebook/comic, archive, and console images; nothing ambiguous ("ts",
/// "bin", "gz" would eat real titles like "Boots" or "Recycle").
const MEDIA_EXTENSIONS: &[&str] = &[
    // video containers
    "mkv", "mp4", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "m2ts", "iso", "vob",
    // audio
    "flac", "mp3", "wav", "ogg", "oga", "m4a", "opus", "wma", "aiff", "ape",
    // books / comics
    "epub", "mobi", "azw3", "pdf", "djvu", "cbz", "cbr",
    // archives
    "zip", "rar", "7z",
    // console images
    "z64", "n64", "gba", "nds", "smc", "sfc", "3ds",
];

/// Release tokens: the key truncates at the FIRST one. Small on purpose — anything
/// after the first hit is dropped wholesale, which is what lets group names go unnamed.
const SCENE_TOKENS: &[&str] = &[
    // resolutions
    "480p", "720p", "1080p", "2160p", "4320p",
    // codecs
    "x264", "x265", "h264", "h265", "hevc", "avc", "xvid", "divx", "10bit",
    // sources / formats
    "bluray", "bdrip", "brrip", "dvdrip", "webrip", "webdl", "hdtv", "remux", "hdr", "hdr10",
    // audio
    "dts", "ac3", "eac3", "aac", "flac", "mp3", "truehd",
    // release tags
    "proper", "repack",
];

/// Normalise a file/folder name into the canonical title key.
///
/// See the module doc for the rule list and the reason each rule exists.
pub fn normalize_title(name: &str) -> String {
    // 1. NFKC, then lowercase (full-width Latin/digits/space become ASCII; case folds).
    let lowered: String = name.nfkc().flat_map(char::to_lowercase).collect();
    // 2. trailing media extension(s), exact-match only.
    let stripped = strip_media_extensions(lowered);
    // 3. bracket groups: [] always noise; () kept only for a bare 1900-2099 year.
    let bracketed = strip_bracket_groups(&stripped);
    // 4. separators: runs of ._ - and whitespace become one space.
    let tokens: Vec<&str> = bracketed
        .split(|c: char| matches!(c, '.' | '_' | '-' | ' ') || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    // 5. truncate at the first scene token.
    let tokens = truncate_scene_noise(tokens);
    // 6. drop a leading article.
    let tokens = drop_leading_article(tokens);
    tokens.join(" ")
}

/// Strip a trailing extension only while it exactly matches `MEDIA_EXTENSIONS`.
/// Loops so ".tar.gz"-style stacks peel too; a name starting with "." keeps its dot.
fn strip_media_extensions(mut s: String) -> String {
    while let Some(dot) = s.rfind('.') {
        let ext = &s[dot + 1..];
        if dot == 0 || !MEDIA_EXTENSIONS.contains(&ext) {
            break;
        }
        s.truncate(dot);
    }
    s
}

/// Remove `[...]` groups outright; keep `(...)` groups only when the inner text is a
/// bare kept year (the year becomes part of the key). Unclosed brackets keep the rest.
fn strip_bracket_groups(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let c = s[i..].chars().next().expect("i is always on a char boundary");
        if c == '(' || c == '[' {
            let close = if c == '(' { ')' } else { ']' };
            match s[i + c.len_utf8()..].find(close) {
                Some(off) => {
                    let end = i + c.len_utf8() + off;
                    let inner = &s[i + c.len_utf8()..end];
                    if c == '(' && is_kept_year(inner) {
                        // KEEP the year — it is part of the identity of the work.
                        // Space-padded so a glued "Dune(1984)" still splits into two tokens.
                        out.push(' ');
                        out.push_str(inner);
                        out.push(' ');
                    }
                    i = end + 1; // `close` is ASCII.
                }
                None => {
                    // Unclosed bracket: keep the rest verbatim rather than guessing.
                    out.push_str(&s[i..]);
                    break;
                }
            }
        } else {
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// True when `inner` is exactly a 4-digit year in 1900..=2099 — the only `(...)` group
/// content that survives stripping, because the year can be part of a work's identity.
fn is_kept_year(inner: &str) -> bool {
    inner.len() == 4
        && inner.bytes().all(|b| b.is_ascii_digit())
        && (1900..=2099).contains(&inner.parse::<u16>().unwrap_or(0))
}

/// Truncate the token list at the FIRST release token (or the "WEB DL" bigram, which
/// the separator pass splits out of "WEB-DL"). A hit at index 0 means the name is only
/// release tokens; keeping it beats erasing it.
fn truncate_scene_noise(tokens: Vec<&str>) -> Vec<&str> {
    for (idx, t) in tokens.iter().enumerate() {
        let hit =
            SCENE_TOKENS.contains(t) || (*t == "web" && tokens.get(idx + 1) == Some(&"dl"));
        if hit {
            if idx == 0 {
                return tokens;
            }
            return tokens[..idx].to_vec();
        }
    }
    tokens
}

/// Drop a leading "the"/"an" (when more than one token) and "a" when the next token is
/// longer than one character (so "A.I." keeps its "A"). Mid-title articles stay.
fn drop_leading_article(tokens: Vec<&str>) -> Vec<&str> {
    if tokens.len() > 1 {
        let first = tokens[0];
        if first == "the" || first == "an" {
            return tokens[1..].to_vec();
        }
        if first == "a" && tokens[1].chars().count() > 1 {
            return tokens[1..].to_vec();
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden vectors: (input, expected key). Real-hoarder shapes — films, TV,
    /// music, games/ISOs, books, non-Latin — plus the deliberate oddballs that pin
    /// the guards (dot-words, year keep, initialism, release-only names).
    /// P-10 mutation: delete the trailing-extension stage (every media-file row reds,
    /// e.g. "DUNE (2021).mp4"); delete the scene-token stage (every release-name row
    /// reds, e.g. row 1); a bypass returning the lowercased input reds nearly all rows.
    #[test]
    fn golden_vectors() {
        let table: &[(&str, &str)] = &[
            // --- films ---
            ("The.Matrix.1999.1080p.BluRay.x264-GRP", "matrix 1999"),
            ("The Matrix (1999).mkv", "matrix 1999"),
            ("the matrix", "matrix"),
            ("Dune (1984) [2160p REMUX]", "dune 1984"),
            ("Dune.Part.Two.2024.2160p.WEB-DL.x265-GRP", "dune part two 2024"),
            ("DUNE (2021).mp4", "dune 2021"),
            // A year glued to the title with no separator still splits off as its own token.
            ("Dune(1984)", "dune 1984"),
            ("Alien (1979).mkv", "alien 1979"),
            ("Aliens (1986).mkv", "aliens 1986"),
            ("Interstellar (2014) REMUX 1080p", "interstellar 2014"),
            ("Blade Runner 2049 2160p WEB-DL DDP5.1 x265-GRP", "blade runner 2049"),
            ("The Godfather (1972) [Criterion Collection].mkv", "godfather 1972"),
            ("Dr. Strangelove (1964).mkv", "dr strangelove 1964"),
            // "Strangelove" is not an extension: the dot-word survives.
            ("Dr. Strangelove.mkv", "dr strangelove"),
            ("2001: A Space Odyssey (1968).mkv", "2001: a space odyssey 1968"),
            ("Movie (2019) (Extended).mkv", "movie 2019"),
            // --- TV: episodes keep their SxxEyy ---
            ("Breaking Bad S01E02 720p WEB-DL", "breaking bad s01e02"),
            ("Game.of.Thrones.S01E10.1080p.BluRay.x264-GRP", "game of thrones s01e10"),
            ("The Office US S02E05.mkv", "office us s02e05"),
            // --- music ---
            ("Pink Floyd - Animals (1977) [FLAC]", "pink floyd animals 1977"),
            ("pink floyd - animals", "pink floyd animals"),
            ("Radiohead - OK Computer (1997) FLAC 320", "radiohead ok computer 1997"),
            ("Nirvana - Nevermind.mp3", "nirvana nevermind"),
            // --- games / ISOs ---
            ("Super Mario Galaxy [NTSC-U].iso", "super mario galaxy"),
            ("Zelda - Ocarina of Time (U) [!].z64", "zelda ocarina of time"),
            ("Doom (1993).zip", "doom 1993"),
            // --- books ---
            ("J.R.R. Tolkien - The Hobbit.epub", "j r r tolkien the hobbit"),
            ("A Game of Thrones - George R.R. Martin.epub", "game of thrones george r r martin"),
            // --- non-Latin / full-width ---
            ("ＡＬＩＥＮ", "alien"),
            ("千と千尋の神隠し.mkv", "千と千尋の神隠し"),
            ("Spirited Away (2001) [BluRay 1080p]", "spirited away 2001"),
            // --- guards and oddballs ---
            ("A.I. Artificial Intelligence (2001).mkv", "a i artificial intelligence 2001"),
            ("A Quiet Place (2018).mp4", "quiet place 2018"),
            ("the 1975", "1975"),
            ("FLAC", "flac"),
            ("1080p", "1080p"),
        ];
        for (input, expected) in table {
            assert_eq!(
                normalize_title(input),
                *expected,
                "input {input:?} normalised wrong"
            );
        }
    }

    /// The same work under different file-shapes collides on one key.
    /// P-10 mutation: removing ANY single pipeline stage must red at least one pair —
    /// NFKC (the ＤＵＮＥ pair), lowercase (same pair), extension strip (Avengers),
    /// bracket strip (Matrix), separator collapse (Nevermind), scene truncation
    /// (Avengers), article strip (Quiet Place).
    #[test]
    fn same_work_collides() {
        let pairs: &[(&str, &str)] = &[
            ("The.Matrix.1999.1080p.BluRay.x264-GRP", "The Matrix (1999).mkv"),
            ("ＤＵＮＥ　１９８４", "Dune (1984)"),
            ("The Avengers (2012).mp4", "Avengers.2012.1080p.x264-GRP"),
            ("A Quiet Place (2018)", "Quiet.Place.2018.1080p.BluRay"),
            ("Nirvana - Nevermind.mp3", "Nirvana.Nevermind"),
        ];
        for (a, b) in pairs {
            assert_eq!(
                normalize_title(a),
                normalize_title(b),
                "{a:?} and {b:?} are the same work but got different keys"
            );
        }
    }

    /// Different works never collide — the year is part of the key, distinct titles
    /// stay distinct.
    /// P-10 mutation: editing `is_kept_year` to return false (dropping "(1984)"
    /// entirely) must red the first three Dune pairs; a prefix-stemming edit that folds
    /// "aliens" onto "alien" must red the Alien pairs.
    #[test]
    fn different_works_stay_apart() {
        let pairs: &[(&str, &str)] = &[
            ("Dune (1984)", "Dune (2021)"),
            ("Dune (1984)", "Dune"),
            ("Dune (2021)", "Dune"),
            ("Alien", "Aliens"),
            ("Alien (1979)", "Aliens (1986)"),
            ("Breaking Bad S01E02", "Breaking Bad S01E03"),
            ("The Matrix", "The Matrix Reloaded"),
        ];
        for (a, b) in pairs {
            assert_ne!(
                normalize_title(a),
                normalize_title(b),
                "{a:?} and {b:?} are different works but collided"
            );
        }
    }

    /// The episode tag is part of the key: noise after it is stripped, the tag itself
    /// never is.
    /// P-10 mutation: adding the SxxEyy shape to `SCENE_TOKENS` must red the equality;
    /// an edit that strips the tag from the key must red the inequality.
    #[test]
    fn episode_tag_is_part_of_the_key() {
        let with_noise = "Breaking Bad S01E02 720p WEB-DL";
        assert_eq!(normalize_title(with_noise), "breaking bad s01e02");
        assert_ne!(
            normalize_title(with_noise),
            normalize_title("Breaking Bad S01E03"),
            "different episodes of the same show collided"
        );
    }

    /// The normaliser is idempotent over every input in the suite — P3 indexes this
    /// output, so a key that changes under re-normalisation would corrupt the index.
    /// P-10 mutation: any edit that makes normalize_title re-mutate its own output
    /// must red this — e.g. unconditionally dropping the last token of the result.
    #[test]
    fn idempotence_over_the_whole_table() {
        let inputs = [
            "The.Matrix.1999.1080p.BluRay.x264-GRP",
            "The Matrix (1999).mkv",
            "the matrix",
            "Dune (1984) [2160p REMUX]",
            "Dune.Part.Two.2024.2160p.WEB-DL.x265-GRP",
            "DUNE (2021).mp4",
            "Alien (1979).mkv",
            "Aliens (1986).mkv",
            "Interstellar (2014) REMUX 1080p",
            "Blade Runner 2049 2160p WEB-DL DDP5.1 x265-GRP",
            "The Godfather (1972) [Criterion Collection].mkv",
            "Dr. Strangelove (1964).mkv",
            "Dr. Strangelove.mkv",
            "2001: A Space Odyssey (1968).mkv",
            "Movie (2019) (Extended).mkv",
            "Breaking Bad S01E02 720p WEB-DL",
            "Game.of.Thrones.S01E10.1080p.BluRay.x264-GRP",
            "The Office US S02E05.mkv",
            "Pink Floyd - Animals (1977) [FLAC]",
            "pink floyd - animals",
            "Radiohead - OK Computer (1997) FLAC 320",
            "Nirvana - Nevermind.mp3",
            "Super Mario Galaxy [NTSC-U].iso",
            "Zelda - Ocarina of Time (U) [!].z64",
            "Doom (1993).zip",
            "J.R.R. Tolkien - The Hobbit.epub",
            "A Game of Thrones - George R.R. Martin.epub",
            "ＡＬＩＥＮ",
            "ＤＵＮＥ　１９８４",
            "千と千尋の神隠し.mkv",
            "Spirited Away (2001) [BluRay 1080p]",
            "A.I. Artificial Intelligence (2001).mkv",
            "A Quiet Place (2018).mp4",
            "the 1975",
            "FLAC",
            "1080p",
        ];
        for input in inputs {
            let once = normalize_title(input);
            let twice = normalize_title(&once);
            assert_eq!(
                once, twice,
                "normalize_title is not idempotent on {input:?}: {once:?} -> {twice:?}"
            );
        }
    }
}
