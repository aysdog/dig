/// Fuzzy subsequence match, case-insensitive: every character of
/// `pattern` must appear in `text` in order, not necessarily touching.
/// Lets typos like "anrban" still find "Anirban".
///
/// Returns a score favoring tighter, earlier matches, or None if the
/// pattern isn't a subsequence of text, or if the match is spread so
/// far across the text that it's more coincidence than a real hit --
/// the span between the first and last matched character can't exceed
/// a few times the pattern's own length.
pub fn fuzzy_score(text: &str, pattern: &str) -> Option<i32> {
    if pattern.is_empty() {
        return Some(0);
    }

    let text_lower: Vec<char> = text.to_lowercase().chars().collect();
    let pattern_lower: Vec<char> = pattern.to_lowercase().chars().collect();

    let mut ti = 0;
    let mut score: i32 = 0;
    let mut first_match: Option<usize> = None;
    let mut last_match: Option<usize> = None;

    for &pc in &pattern_lower {
        let mut found = false;
        while ti < text_lower.len() {
            let tc = text_lower[ti];
            let hit = tc == pc;
            let pos = ti;
            ti += 1;
            if hit {
                found = true;
                match last_match {
                    Some(last) if pos == last + 1 => score += 6, // consecutive run bonus
                    Some(last) => score -= (pos - last) as i32, // gap penalty
                    None => score += 10 - (pos as i32).min(10),  // prefer an early start
                }
                first_match.get_or_insert(pos);
                last_match = Some(pos);
                break;
            }
        }
        if !found {
            return None;
        }
    }

    const MAX_SPAN_MULTIPLIER: usize = 4;
    let span = last_match.unwrap() - first_match.unwrap() + 1;
    if span > pattern_lower.len() * MAX_SPAN_MULTIPLIER {
        return None; // matched characters too scattered to be a real hit
    }

    Some(score)
}

/// Every word in `words` must fuzzy-match somewhere in `text`; the
/// per-word scores are summed so tighter overall matches rank higher.
pub fn fuzzy_match_all(text: &str, words: &[String]) -> Option<i32> {
    let mut total = 0;
    for w in words {
        match fuzzy_score(text, w) {
            Some(s) => total += s,
            None => return None,
        }
    }
    Some(total)
}

/// Strict, literal match. Whoever calls this decides casing by what
/// they pass in -- lowercase both sides for case-insensitive, or pass
/// them as-is for case-sensitive. Used by `dig -f -c`.
pub fn exact_match_all(text: &str, words: &[String]) -> bool {
    words.iter().all(|w| text.contains(w.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_still_scores() {
        assert!(fuzzy_score("Anirban", "anirban").is_some());
    }

    #[test]
    fn tolerates_a_missing_letter() {
        // "anrban" is missing the 'i' from "Anirban"
        assert!(fuzzy_score("Anirban", "anrban").is_some());
    }

    #[test]
    fn tolerates_a_dropped_trailing_letter() {
        // "anirbn" is missing the final 'a' from "Anirban"
        assert!(fuzzy_score("Anirban", "anirbn").is_some());
    }

    #[test]
    fn rejects_letters_out_of_order() {
        // 'z' never appears at all
        assert!(fuzzy_score("Anirban", "anirbaz").is_none());
    }

    #[test]
    fn rejects_when_pattern_longer_than_text() {
        assert!(fuzzy_score("hi", "hithere").is_none());
    }

    #[test]
    fn tighter_match_scores_higher_than_scattered_match() {
        let tight = fuzzy_score("resume.pdf", "resume").unwrap();
        let scattered = fuzzy_score("r-e-s-u-m-e-report.pdf", "resume").unwrap();
        assert!(tight > scattered);
    }

    #[test]
    fn empty_pattern_matches_everything_with_zero_score() {
        assert_eq!(fuzzy_score("anything", ""), Some(0));
    }

    #[test]
    fn multi_word_all_must_match() {
        let words = vec!["resume".to_string(), "pdf".to_string()];
        assert!(fuzzy_match_all("Anirban_Resume.pdf", &words).is_some());
        let words2 = vec!["resume".to_string(), "docx".to_string()];
        assert!(fuzzy_match_all("Anirban_Resume.pdf", &words2).is_none());
    }

    #[test]
    fn exact_match_is_literal_and_case_sensitive_when_given_as_is() {
        // caller decides casing now -- pass raw text and raw words to
        // get case-sensitive behavior
        assert!(!exact_match_all("Anirban", &["anir".to_string()]));
        assert!(exact_match_all("Anirban", &["Anir".to_string()]));
    }

    #[test]
    fn exact_match_is_case_insensitive_when_caller_lowercases_both() {
        // this is what find.rs does for the default (no -c) path
        assert!(exact_match_all("anirban", &["anir".to_string()]));
        assert!(!exact_match_all("anirban", &["anrban".to_string()])); // still not a substring
    }

    #[test]
    fn rejects_wildly_scattered_coincidental_match() {
        // "abc" as a subsequence exists in this text but only by
        // scattering across the whole thing -- not a real match.
        let text = "a_______________________________b_______________________________c";
        assert!(fuzzy_score(text, "abc").is_none());
    }

    #[test]
    fn still_accepts_a_realistic_typo_gap() {
        // one dropped character in an otherwise tight match must still pass
        assert!(fuzzy_score("Anirban", "anrban").is_some());
    }

    #[test]
    fn accepts_a_match_spread_across_a_reasonably_long_filename() {
        // pattern "resume" inside a longer but still tight filename
        assert!(fuzzy_score("my_resume_final_v2.pdf", "resume").is_some());
    }
}
