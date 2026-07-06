use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Equal(String),
    Delete(String),
    Insert(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub lines: Vec<DiffLine>,
}

pub fn hunks(reviewed: &str, current: &str) -> Vec<Hunk> {
    let diff = TextDiff::from_lines(reviewed, current);
    diff.grouped_ops(3)
        .into_iter()
        .map(|group| {
            let mut lines = Vec::new();
            for op in group {
                for change in diff.iter_changes(&op) {
                    let line = change.to_string();
                    match change.tag() {
                        ChangeTag::Equal => lines.push(DiffLine::Equal(line)),
                        ChangeTag::Delete => lines.push(DiffLine::Delete(line)),
                        ChangeTag::Insert => lines.push(DiffLine::Insert(line)),
                    }
                }
            }
            Hunk { lines }
        })
        .filter(|hunk| {
            hunk.lines
                .iter()
                .any(|line| !matches!(line, DiffLine::Equal(_)))
        })
        .collect()
}

pub fn apply_hunk(reviewed: &str, hunk: &Hunk) -> String {
    let mut output = String::new();
    let mut hunk_started = false;
    let mut old_iter = reviewed.split_inclusive('\n').peekable();

    for line in &hunk.lines {
        match line {
            DiffLine::Equal(text) => {
                copy_until(&mut old_iter, text, &mut output);
                output.push_str(text);
                let _ = old_iter.next();
                hunk_started = true;
            }
            DiffLine::Delete(text) => {
                copy_until(&mut old_iter, text, &mut output);
                let _ = old_iter.next();
                hunk_started = true;
            }
            DiffLine::Insert(text) => {
                if !hunk_started {
                    hunk_started = true;
                }
                output.push_str(text);
            }
        }
    }

    for rest in old_iter {
        output.push_str(rest);
    }

    output
}

fn copy_until<'a, I>(old_iter: &mut std::iter::Peekable<I>, needle: &str, output: &mut String)
where
    I: Iterator<Item = &'a str>,
{
    while let Some(next) = old_iter.peek() {
        if *next == needle {
            break;
        }
        output.push_str(next);
        let _ = old_iter.next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_insert_hunk() {
        let old = "a\nc\n";
        let new = "a\nb\nc\n";
        let h = hunks(old, new);
        assert_eq!(apply_hunk(old, &h[0]), new);
    }

    #[test]
    fn apply_delete_hunk() {
        let old = "a\nb\nc\n";
        let new = "a\nc\n";
        let h = hunks(old, new);
        assert_eq!(apply_hunk(old, &h[0]), new);
    }

    #[test]
    fn apply_replace_hunk() {
        let old = "a\nb\nc\n";
        let new = "a\nx\nc\n";
        let h = hunks(old, new);
        assert_eq!(apply_hunk(old, &h[0]), new);
    }

    #[test]
    fn accepting_one_hunk_leaves_other_delta() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\no\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\nO\n";
        let h = hunks(old, new);
        assert_eq!(h.len(), 2);
        let partially = apply_hunk(old, &h[0]);
        assert_ne!(partially, new);
        assert_eq!(hunks(&partially, new).len(), 1);
    }
}
