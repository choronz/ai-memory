//! Markdown → HTML rendering with `syntect` syntax highlighting.
//!
//! Stub at scaffold time; full implementation in the next step.

use pulldown_cmark::{Options, Parser, html};

/// Render a markdown body to HTML using GFM-ish defaults.
///
/// v1: trust the wiki source (the wiki is on-disk markdown the
/// project owner writes/consolidates; not user-uploaded content from
/// arbitrary callers). Syntax highlighting is deferred.
#[must_use]
pub fn render(body: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_SMART_PUNCTUATION);
    let parser = Parser::new_ext(body, opts);
    let mut out = String::with_capacity(body.len() + body.len() / 4);
    html::push_html(&mut out, parser);
    out
}

/// Drop the leading H1 from a markdown body if present. Static-site
/// convention: the first H1 IS the page title, and the page template
/// already renders the title in its header — leaving it in the body
/// duplicates it on screen. No-op when the body doesn't start with
/// an H1 (handles `# Title`, both ATX `# Title` and setext
/// `Title\n=====` forms).
#[must_use]
pub fn strip_leading_h1(body: &str) -> &str {
    // Skip any leading blank lines.
    let trimmed = body.trim_start_matches(['\n', '\r']);
    // ATX form: `# Title` (one `#`, NOT `## …`).
    if let Some(rest) = trimmed.strip_prefix("# ") {
        let after_line = rest.find('\n').map_or("", |nl| &rest[nl + 1..]);
        return after_line.trim_start_matches(['\n', '\r']);
    }
    // Setext form: `Title\n====…` (1+ equals signs). Look ahead.
    if let Some((first_line, after_first)) = trimmed.split_once('\n')
        && !first_line.is_empty()
        && let Some((second_line, after_second)) = after_first.split_once('\n')
        && !second_line.is_empty()
        && second_line.chars().all(|c| c == '=')
    {
        return after_second.trim_start_matches(['\n', '\r']);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_basic_markdown() {
        let html = render("# Hello\n\nworld");
        assert!(html.contains("<h1>Hello</h1>"));
        assert!(html.contains("<p>world</p>"));
    }

    #[test]
    fn renders_tables() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |";
        let html = render(md);
        assert!(html.contains("<table>"));
        assert!(html.contains("<td>1</td>"));
    }

    #[test]
    fn strip_atx_h1_drops_first_heading() {
        let out = strip_leading_h1("# Title\n\nbody text\n");
        assert_eq!(out, "body text\n");
    }

    #[test]
    fn strip_atx_h1_tolerates_leading_blank_lines() {
        let out = strip_leading_h1("\n\n# Title\n\nbody\n");
        assert_eq!(out, "body\n");
    }

    #[test]
    fn strip_atx_h1_leaves_h2_alone() {
        let out = strip_leading_h1("## Subhead\n\nbody\n");
        assert_eq!(out, "## Subhead\n\nbody\n");
    }

    #[test]
    fn strip_atx_h1_leaves_body_without_title_alone() {
        let out = strip_leading_h1("just a paragraph\n");
        assert_eq!(out, "just a paragraph\n");
    }

    #[test]
    fn strip_setext_h1_drops_first_heading() {
        let out = strip_leading_h1("Title\n=====\n\nbody\n");
        assert_eq!(out, "body\n");
    }

    #[test]
    fn strip_does_not_eat_setext_h2() {
        // `----` underlines are H2, not H1. Leave them alone.
        let out = strip_leading_h1("Title\n----\n\nbody\n");
        assert_eq!(out, "Title\n----\n\nbody\n");
    }
}
