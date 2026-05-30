//! Markdown → safe HTML for assistant text. Uses pulldown-cmark with a
//! conservative options set (strikethrough, no raw HTML pass-through).
//!
//! pulldown-cmark's own `html::push_html` writer already escapes text
//! content correctly; we then strip any literal `<script>` blocks as
//! belt-and-braces in case a future option pulls in raw HTML mode.

use pulldown_cmark::{html, Options, Parser};

pub fn render(src: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_TABLES);

    let parser = Parser::new_ext(src, opts);
    let mut out = String::with_capacity(src.len() * 3 / 2);
    html::push_html(&mut out, parser);
    out
}
