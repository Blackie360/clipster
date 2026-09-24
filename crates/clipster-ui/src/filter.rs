//! Fuzzy matching for the picker's search box.
//!
//! A subsequence matcher with a handful of bonuses, not a port of fzf. The
//! corpus is a few hundred single-line previews and the query is typed by
//! hand, so the scoring only has to be good enough to put the obvious answer
//! first — and being 60 lines of std is worth more here than being optimal.

/// Score `haystack` against `needle`, or `None` if it does not match.
///
/// Higher is better. Matching is case-insensitive, and whitespace in the
/// query is ignored so "git com" finds "git commit" without the space having
/// to line up.
pub fn score(haystack: &str, needle: &str) -> Option<i32> {
    if needle.trim().is_empty() {
        return Some(0);
    }

    let hay: Vec<char> = haystack.chars().flat_map(char::to_lowercase).collect();
    let mut score = 0;
    let mut cursor = 0;
    let mut previous: Option<usize> = None;

    for want in needle.chars().flat_map(char::to_lowercase) {
        if want.is_whitespace() {
            continue;
        }
        let at = cursor + hay[cursor..].iter().position(|&c| c == want)?;

        score += 10;
        // Runs of adjacent characters are what distinguishes a real substring
        // hit from letters scattered across the line. The distance skipped to
        // reach a character costs, or "gitcom" ranks a github.com URL above
        // the `git commit` it was obviously typed to find: both match in two
        // tight runs, and only the gap between them tells them apart.
        match previous {
            Some(prev) if prev + 1 == at => score += 15,
            Some(prev) => score -= 2 * (at - prev - 1).min(20) as i32,
            None => {}
        }
        // A match at the start of a word is usually what the user meant.
        if at == 0 || !hay[at - 1].is_alphanumeric() {
            score += 8;
        }

        previous = Some(at);
        cursor = at + 1;
    }

    // Prefer the entry that finished matching earliest: "cargo" should rank
    // the line that starts with it above one that mentions it in passing.
    Some(score - (cursor as i32) / 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_matches_are_rejected() {
        assert_eq!(score("git commit", "xyz"), None);
        // Right characters, wrong order.
        assert_eq!(score("abc", "cba"), None);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(score("Git Commit", "git").is_some());
        assert!(score("git commit", "GIT").is_some());
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert_eq!(score("anything", ""), Some(0));
        assert_eq!(score("anything", "   "), Some(0));
    }

    #[test]
    fn contiguous_beats_scattered() {
        // The scattered haystack has to actually contain the needle as a
        // subsequence, two "m"s included, or this compares against a None.
        let contiguous = score("git commit", "commit").unwrap();
        let scattered = score("cargo omit my item", "commit").unwrap();
        assert!(contiguous > scattered, "{contiguous} !> {scattered}");
    }

    #[test]
    fn an_earlier_match_beats_a_later_one() {
        let early = score("commit template", "commit").unwrap();
        let late = score("the usual commit", "commit").unwrap();
        assert!(early > late, "{early} !> {late}");
    }

    #[test]
    fn a_tight_match_beats_one_spread_across_the_line() {
        // Observed in the picker: "gitcom" hits both of these in two runs,
        // and without a gap penalty the URL edged out the obvious answer.
        let commit = score("commit template git commit --amend --no-edit", "gitcom").unwrap();
        let url = score("https://github.com/rust-lang/rust/pull/128432", "gitcom").unwrap();
        assert!(commit > url, "{commit} !> {url}");
    }

    #[test]
    fn whitespace_in_the_query_is_ignored() {
        assert!(score("gitcommit", "git com").is_some());
    }

    #[test]
    fn multibyte_content_does_not_panic() {
        assert!(score("héllo wörld", "wor").is_none());
        assert!(score("héllo wörld", "hello").is_none());
        assert!(score("héllo wörld", "hll").is_some());
    }
}
