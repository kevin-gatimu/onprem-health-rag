//! Streaming filter that strips `<think>...</think>` spans (Qwen3 reasoning blocks,
//! including the empty block emitted under `/no_think`) from a token stream.

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// Streaming filter that strips `<think>...</think>` spans (Qwen3 reasoning blocks,
/// including the empty block emitted under `/no_think`) from a token stream.
///
/// Tokens arrive as fragments, so an opening or closing tag may be split across two
/// pushes (e.g. `"<thi"` then `"nk>"`). The filter buffers only the minimum trailing
/// text that could still be the prefix of a tag, so everything else streams out
/// immediately with no added latency.
#[derive(Default)]
pub struct ThinkFilter {
    buf: String,
    in_think: bool,
}

impl ThinkFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `fragment` to the internal buffer, then drain the extraction loop,
    /// returning the safe-to-emit visible text (may be empty).
    pub fn push(&mut self, fragment: &str) -> String {
        self.buf.push_str(fragment);
        self.drain()
    }

    /// Flush at end of stream: if NOT currently inside a think block, emit whatever
    /// remains in the buffer (it was a trailing partial-tag prefix that never completed
    /// — real text); if inside an unterminated think block, emit nothing (drop it).
    pub fn finish(&mut self) -> String {
        if self.in_think {
            self.buf.clear();
            String::new()
        } else {
            let out = self.buf.clone();
            self.buf.clear();
            out
        }
    }

    fn drain(&mut self) -> String {
        let mut out = String::new();
        loop {
            if !self.in_think {
                if let Some(i) = self.buf.find(OPEN) {
                    out.push_str(&self.buf[..i]); // text before the tag is real
                    self.buf.drain(..i + OPEN.len()); // consume up to and including "<think>"
                    self.in_think = true;
                    continue;
                } else {
                    // No full OPEN. Emit all but the longest trailing suffix of buf that is a
                    // PROPER prefix of OPEN (it might be the start of a split tag).
                    let keep = longest_partial_prefix(&self.buf, OPEN);
                    let cut = self.buf.len() - keep;
                    out.push_str(&self.buf[..cut]);
                    self.buf.drain(..cut); // retain only the possible partial tag
                    break;
                }
            } else {
                if let Some(i) = self.buf.find(CLOSE) {
                    self.buf.drain(..i + CLOSE.len()); // drop think content + "</think>"
                    self.in_think = false;
                    continue;
                } else {
                    // Still inside think: drop everything except a possible trailing partial
                    // prefix of CLOSE (so a split "</thi|nk>" is caught on the next push).
                    let keep = longest_partial_prefix(&self.buf, CLOSE);
                    let cut = self.buf.len() - keep;
                    self.buf.drain(..cut); // dropped (think content), emit nothing
                    break;
                }
            }
        }
        out
    }
}

/// Largest k in 1..tag.len() such that `buf` ends with `tag[..k]` — the longest
/// trailing fragment of `buf` that could be the start of `tag`. Returns 0 if none.
/// Only PROPER prefixes (k < tag.len()); a full occurrence is handled by `find`.
fn longest_partial_prefix(buf: &str, tag: &str) -> usize {
    let max = buf.len().min(tag.len() - 1);
    for k in (1..=max).rev() {
        if buf.is_char_boundary(buf.len() - k) && buf[buf.len() - k..] == tag[..k] {
            return k;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::ThinkFilter;

    fn run(pushes: &[&str]) -> String {
        let mut f = ThinkFilter::new();
        let mut out = String::new();
        for p in pushes {
            out.push_str(&f.push(p));
        }
        out.push_str(&f.finish());
        out
    }

    #[test]
    fn empty_think_block() {
        assert_eq!(run(&["<think>\n\n</think>Hello world"]), "Hello world");
    }

    #[test]
    fn non_empty_block() {
        assert_eq!(run(&["<think>reasoning here</think>Answer"]), "Answer");
    }

    #[test]
    fn open_tag_split_across_pushes() {
        assert_eq!(run(&["<thi", "nk>x</think>Hi"]), "Hi");
    }

    #[test]
    fn close_tag_split_across_pushes() {
        assert_eq!(run(&["<think>abc</thi", "nk>Done"]), "Done");
    }

    #[test]
    fn no_tags() {
        assert_eq!(run(&["Just plain text."]), "Just plain text.");
    }

    #[test]
    fn text_before_block_preserved() {
        assert_eq!(run(&["Pre <think>x</think> Post"]), "Pre  Post");
    }

    #[test]
    fn lone_angle_bracket_not_swallowed() {
        assert_eq!(run(&["a < b is true"]), "a < b is true");
    }

    #[test]
    fn unterminated_think_block_at_end() {
        assert_eq!(run(&["<think>never closes"]), "");
    }

    #[test]
    fn fragment_by_fragment() {
        let input = "<think>hi</think>OK";
        let pushes: Vec<&str> = input
            .char_indices()
            .map(|(i, c)| &input[i..i + c.len_utf8()])
            .collect();
        let result = run(&pushes);
        assert_eq!(result, "OK");
    }
}
