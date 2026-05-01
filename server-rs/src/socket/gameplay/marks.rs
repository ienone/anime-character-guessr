use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    // Attempt marks: count towards maxAttempts
    static ref ATTEMPT_MARK_RE: Regex = Regex::new(r"(?:⏱️?|💡|✔|❌)").unwrap();
    // End marks: indicate the player/team has ended the round
    static ref END_MARK_RE: Regex = Regex::new(r"[✌👑💀🏆🏳️]").unwrap();
}

pub fn count_attempt_marks(marks: &str) -> usize {
    ATTEMPT_MARK_RE.find_iter(marks).count()
}

pub fn has_end_mark(marks: &str) -> bool {
    END_MARK_RE.is_match(marks)
}

pub fn strip_end_marks(marks: &str) -> String {
    END_MARK_RE.replace_all(marks, "").to_string()
}

pub fn append_end_mark_once(marks: &str, end_mark: &str) -> String {
    let stripped = strip_end_marks(marks);
    format!("{}{}", stripped, end_mark)
}
