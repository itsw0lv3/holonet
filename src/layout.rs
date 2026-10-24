use std::collections::HashSet;
use std::ops::Deref;

use ego_tree::NodeId;
use scraper::{ElementRef, Html, Node};
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleKind {
    Text,
    Heading,
    Link,
    Code,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    pub style: StyleKind,
    pub focus: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub spans: Vec<Piece>,
}

#[derive(Clone, Debug)]
pub enum Focus {
    Link { url: String },
    Field { form: usize, field: usize },
    Submit { form: usize, field: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Hidden,
    Submit,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub value: String,
    pub kind: FieldKind,
}

#[derive(Clone, Debug)]
pub struct Form {
    pub action: String,
    pub method_post: bool,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub title: String,
    pub lines: Vec<Line>,
    pub focus: Vec<Focus>,
    pub forms: Vec<Form>,
    pub text_chars: usize,
}

pub struct Submission {
    pub url: String,
    pub post: bool,
    pub body: Option<String>,
}

pub fn layout(
    html: &Html,
    page_url: &str,
    width: usize,
    hidden: &HashSet<NodeId>,
) -> (Document, u32) {
    let width = width.max(1);
    let base =
        Url::parse(page_url).unwrap_or_else(|_| Url::parse("https://example.invalid/").unwrap());
    let mut ctx = Ctx {
        lines: Vec::new(),
        cur: Vec::new(),
        col: 0,
        width,
        pre: 0,
        in_cell: false,
        text_chars: 0,
        focus: Vec::new(),
        forms: Vec::new(),
        base,
        title: String::new(),
        hidden,
        hidden_count: 0,
        link_focus: None,
        heading: false,
        code: 0,
        current_form: None,
        ol: Vec::new(),
    };
    if let Ok(base_sel) = scraper::Selector::parse("base[href]")
        && let Some(base_el) = html.select(&base_sel).next()
        && let Some(href) = base_el.attr("href")
        && let Ok(joined) = ctx.base.join(href)
    {
        ctx.base = joined;
    }
    ctx.walk(html.root_element());
    ctx.finish(false);
    let text_chars = ctx.text_chars;
    (
        Document {
            title: ctx.title,
            lines: ctx.lines,
            focus: ctx.focus,
            forms: ctx.forms,
            text_chars,
        },
        ctx.hidden_count,
    )
}

pub fn submission(doc: &Document, form_index: usize, submit: Option<usize>) -> Option<Submission> {
    let form = doc.forms.get(form_index)?;
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (index, field) in form.fields.iter().enumerate() {
        if field.name.is_empty() {
            continue;
        }
        let include = match field.kind {
            FieldKind::Text | FieldKind::Hidden => true,
            FieldKind::Submit => submit == Some(index),
        };
        if include {
            serializer.append_pair(&field.name, &field.value);
        }
    }
    let encoded = serializer.finish();
    if form.method_post {
        Some(Submission {
            url: form.action.clone(),
            post: true,
            body: Some(encoded),
        })
    } else if encoded.is_empty() {
        Some(Submission {
            url: form.action.clone(),
            post: false,
            body: None,
        })
    } else if form.action.contains('?') {
        Some(Submission {
            url: format!("{}&{encoded}", form.action),
            post: false,
            body: None,
        })
    } else {
        Some(Submission {
            url: format!("{}?{encoded}", form.action),
            post: false,
            body: None,
        })
    }
}

struct Ctx<'a> {
    lines: Vec<Line>,
    cur: Vec<Piece>,
    col: usize,
    width: usize,
    pre: u32,
    in_cell: bool,
    text_chars: usize,
    focus: Vec<Focus>,
    forms: Vec<Form>,
    base: Url,
    title: String,
    hidden: &'a HashSet<NodeId>,
    hidden_count: u32,
    link_focus: Option<usize>,
    heading: bool,
    code: u32,
    current_form: Option<usize>,
    ol: Vec<u32>,
}

impl Ctx<'_> {
    fn walk(&mut self, el: ElementRef) {
        if self.hidden.contains(&el.id()) {
            self.hidden_count += 1;
            return;
        }
        let name = el.value().name();
        match name {
            "script" | "style" | "noscript" | "iframe" | "svg" | "template" => {}
            "head" => self.walk_children(el),
            "title" => {
                if self.title.is_empty() {
                    self.title = el
                        .text()
                        .collect::<String>()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                }
            }
            "br" => self.finish(false),
            "hr" => {
                self.break_block();
                self.push_text("---", StyleKind::Text, None);
                self.break_block();
            }
            "pre" => {
                self.break_block();
                self.pre += 1;
                self.walk_children(el);
                self.pre -= 1;
                self.break_block();
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.break_block();
                self.heading = true;
                self.walk_children(el);
                self.heading = false;
                self.break_block();
            }
            "a" => self.walk_link(el),
            "img" => self.walk_img(el),
            "table" => self.walk_table(el),
            "form" => self.walk_form(el),
            "input" => self.walk_input(el),
            "textarea" => self.walk_textarea(el),
            "button" => self.walk_button(el),
            "select" => self.walk_select(el),
            "ol" => {
                self.ol.push(0);
                self.break_block();
                self.walk_children(el);
                self.ol.pop();
            }
            "li" => self.walk_li(el),
            "code" | "kbd" => {
                self.code += 1;
                self.walk_children(el);
                self.code -= 1;
            }
            "p" | "div" | "section" | "article" | "header" | "footer" | "main" | "nav"
            | "blockquote" | "ul" | "dl" | "dt" | "dd" | "figure" | "figcaption" | "address"
            | "aside" | "hgroup" => {
                self.break_block();
                self.walk_children(el);
                self.break_block();
            }
            _ => self.walk_children(el),
        }
    }

    fn walk_children(&mut self, el: ElementRef) {
        for child in el.children() {
            match child.value() {
                Node::Text(text) => self.push_text(text.deref(), self.style(), self.link_focus),
                Node::Element(_) => {
                    if let Some(child) = ElementRef::wrap(child) {
                        self.walk(child);
                    }
                }
                _ => {}
            }
        }
    }

    fn walk_link(&mut self, el: ElementRef) {
        let Some(href) = el.attr("href") else {
            self.walk_children(el);
            return;
        };
        let Some(url) = self.absolute(href) else {
            self.walk_children(el);
            return;
        };
        let index = self.focus.len();
        self.focus.push(Focus::Link { url: url.clone() });
        let previous = self.link_focus.replace(index);
        let before = self.text_chars;
        self.walk_children(el);
        if self.text_chars == before {
            self.push_text(&url, StyleKind::Link, Some(index));
        }
        self.link_focus = previous;
    }

    // shortcut: images are alt text that may link to the source, upgrade when painting with the Kitty protocol
    fn walk_img(&mut self, el: ElementRef) {
        let alt = el
            .attr("alt")
            .filter(|alt| !alt.is_empty())
            .unwrap_or("[image]");
        if let Some(src) = el.attr("src").and_then(|src| self.absolute(src)) {
            let index = self.focus.len();
            self.focus.push(Focus::Link { url: src });
            self.push_text(alt, StyleKind::Link, Some(index));
        } else {
            self.push_text(alt, self.style(), self.link_focus);
        }
    }

    fn walk_table(&mut self, el: ElementRef) {
        self.break_block();
        for child in el.children() {
            let Some(child) = ElementRef::wrap(child) else {
                continue;
            };
            match child.value().name() {
                "tr" => self.walk_row(child),
                "thead" | "tbody" | "tfoot" => {
                    for row in child.children() {
                        if let Some(row) = ElementRef::wrap(row)
                            && row.value().name() == "tr"
                        {
                            self.walk_row(row);
                        }
                    }
                }
                _ => {}
            }
        }
        self.break_block();
    }

    fn walk_row(&mut self, el: ElementRef) {
        self.break_block();
        let mut first = true;
        for child in el.children() {
            let Some(child) = ElementRef::wrap(child) else {
                continue;
            };
            if !matches!(child.value().name(), "td" | "th") {
                continue;
            }
            if !first {
                self.push_text(" | ", StyleKind::Text, None);
            }
            first = false;
            let was = self.in_cell;
            self.in_cell = true;
            self.walk_children(child);
            self.in_cell = was;
        }
        self.break_block();
    }

    fn walk_form(&mut self, el: ElementRef) {
        let action = el
            .attr("action")
            .and_then(|href| self.absolute(href))
            .unwrap_or_else(|| self.base.to_string());
        let method_post = el
            .attr("method")
            .is_some_and(|method| method.eq_ignore_ascii_case("post"));
        let index = self.forms.len();
        self.forms.push(Form {
            action,
            method_post,
            fields: Vec::new(),
        });
        let previous = self.current_form.replace(index);
        self.break_block();
        self.walk_children(el);
        self.break_block();
        self.current_form = previous;
    }

    fn walk_input(&mut self, el: ElementRef) {
        let kind = el.attr("type").unwrap_or("text").to_ascii_lowercase();
        let name = el.attr("name").unwrap_or("").to_string();
        let value = el.attr("value").unwrap_or("").to_string();
        match kind.as_str() {
            "hidden" => {
                self.add_field(name, value, FieldKind::Hidden);
            }
            "submit" | "image" => self.add_submit(
                name,
                if value.is_empty() {
                    "submit".into()
                } else {
                    value
                },
            ),
            "button" | "reset" | "checkbox" | "radio" | "file" => {}
            _ => {
                let shown = if value.is_empty() {
                    name.clone()
                } else {
                    value.clone()
                };
                let field = self.add_field(name, value, FieldKind::Text);
                let form = self.current_form.unwrap();
                let index = self.focus.len();
                self.focus.push(Focus::Field { form, field });
                self.push_text(&format!("[{shown}]"), StyleKind::Text, Some(index));
            }
        }
    }

    fn walk_textarea(&mut self, el: ElementRef) {
        let name = el.attr("name").unwrap_or("").to_string();
        let value = el.text().collect::<String>();
        let shown = if value.is_empty() {
            name.clone()
        } else {
            value.clone()
        };
        let field = self.add_field(name, value, FieldKind::Text);
        let form = self.current_form.unwrap();
        let index = self.focus.len();
        self.focus.push(Focus::Field { form, field });
        self.push_text(&format!("[{shown}]"), StyleKind::Text, Some(index));
    }

    fn walk_button(&mut self, el: ElementRef) {
        let kind = el.attr("type").unwrap_or("submit");
        if kind.eq_ignore_ascii_case("button") || kind.eq_ignore_ascii_case("reset") {
            return;
        }
        let name = el.attr("name").unwrap_or("").to_string();
        let value = el
            .attr("value")
            .map(str::to_string)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| el.text().collect());
        let value = if value.is_empty() {
            "submit".into()
        } else {
            value
        };
        self.add_submit(name, value);
    }

    // shortcut: a select submits its selected option and cannot be changed, upgrade if a form needs another choice
    fn walk_select(&mut self, el: ElementRef) {
        let name = el.attr("name").unwrap_or("").to_string();
        let mut chosen = String::new();
        let mut label = String::new();
        for child in el.descendants() {
            let Some(option) = ElementRef::wrap(child) else {
                continue;
            };
            if option.value().name() != "option" {
                continue;
            }
            let value = option
                .attr("value")
                .map(str::to_string)
                .unwrap_or_else(|| option.text().collect());
            let text = option.text().collect::<String>();
            if chosen.is_empty() || option.value().attr("selected").is_some() {
                chosen = value;
                label = text;
            }
        }
        if !name.is_empty() {
            self.add_field(name, chosen, FieldKind::Hidden);
        }
        if !label.is_empty() {
            self.push_text(&format!("[{label}]"), self.style(), self.link_focus);
        }
    }

    fn walk_li(&mut self, el: ElementRef) {
        self.break_block();
        let prefix = if let Some(n) = self.ol.last_mut() {
            *n += 1;
            format!("{n}. ")
        } else {
            "- ".into()
        };
        self.push_text(&prefix, StyleKind::Text, None);
        self.walk_children(el);
        self.break_block();
    }

    fn add_field(&mut self, name: String, value: String, kind: FieldKind) -> usize {
        let form = self.ensure_form();
        let field = self.forms[form].fields.len();
        self.forms[form].fields.push(Field { name, value, kind });
        field
    }

    fn add_submit(&mut self, name: String, value: String) {
        let field = self.add_field(name, value.clone(), FieldKind::Submit);
        let form = self.current_form.unwrap();
        let index = self.focus.len();
        self.focus.push(Focus::Submit { form, field });
        self.push_text(&format!("[{value}]"), StyleKind::Text, Some(index));
    }

    fn ensure_form(&mut self) -> usize {
        if let Some(index) = self.current_form {
            return index;
        }
        let index = self.forms.len();
        self.forms.push(Form {
            action: self.base.to_string(),
            method_post: false,
            fields: Vec::new(),
        });
        self.current_form = Some(index);
        index
    }

    fn absolute(&self, href: &str) -> Option<String> {
        self.base.join(href).ok().map(|url| url.to_string())
    }

    fn style(&self) -> StyleKind {
        if self.link_focus.is_some() {
            StyleKind::Link
        } else if self.heading {
            StyleKind::Heading
        } else if self.code > 0 {
            StyleKind::Code
        } else {
            StyleKind::Text
        }
    }

    fn break_block(&mut self) {
        if self.in_cell {
            if self.col > 0 && !self.ends_space() {
                self.push_text(" ", StyleKind::Text, None);
            }
            return;
        }
        self.finish(false);
    }

    fn push_text(&mut self, text: &str, style: StyleKind, focus: Option<usize>) {
        if self.pre > 0 {
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    self.finish(true);
                }
                self.push_hard(line, style, focus);
            }
            return;
        }
        let flat = collapse_ws(text);
        if flat.is_empty() {
            return;
        }
        let mut flat = flat;
        if flat.starts_with(' ') && (self.cur.is_empty() || self.ends_space()) {
            flat.remove(0);
        }
        if flat.is_empty() {
            return;
        }
        self.push_words(&flat, style, focus);
    }

    fn push_words(&mut self, text: &str, style: StyleKind, focus: Option<usize>) {
        let mut token = String::new();
        let mut tokens = Vec::new();
        for ch in text.chars() {
            if ch == ' ' {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                tokens.push(" ".into());
            } else {
                token.push(ch);
            }
        }
        if !token.is_empty() {
            tokens.push(token);
        }
        for token in tokens {
            if token == " " {
                if self.col == 0 || self.col >= self.width {
                    continue;
                }
                self.add(" ", style, focus);
                continue;
            }
            let width = token.width();
            if self.col > 0 && self.col + width > self.width {
                self.finish(false);
            }
            if token.width() > self.width {
                self.push_hard(&token, style, focus);
            } else {
                self.add(&token, style, focus);
            }
        }
    }

    // shortcut: preformatted lines wrap at the terminal width, upgrade if a page needs horizontal scroll
    fn push_hard(&mut self, text: &str, style: StyleKind, focus: Option<usize>) {
        let mut buf = String::new();
        let mut width = 0;
        for ch in text.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if self.col + width + ch_width > self.width && self.col + width > 0 {
                self.add(&buf, style, focus);
                buf.clear();
                width = 0;
                self.finish(self.pre > 0);
            }
            buf.push(ch);
            width += ch_width;
        }
        if !buf.is_empty() {
            self.add(&buf, style, focus);
        }
    }

    fn add(&mut self, text: &str, style: StyleKind, focus: Option<usize>) {
        if text.is_empty() {
            return;
        }
        self.text_chars += text.chars().filter(|ch| !ch.is_whitespace()).count();
        self.col += text.width();
        if let Some(last) = self.cur.last_mut()
            && last.style == style
            && last.focus == focus
        {
            last.text.push_str(text);
            return;
        }
        self.cur.push(Piece {
            text: text.to_string(),
            style,
            focus,
        });
    }

    fn finish(&mut self, keep_space: bool) {
        if self.cur.is_empty() {
            return;
        }
        if !keep_space
            && let Some(last) = self.cur.last_mut()
            && last.text.ends_with(' ')
        {
            last.text.pop();
            if last.text.is_empty() {
                self.cur.pop();
            }
        }
        if self.cur.is_empty() {
            self.col = 0;
            return;
        }
        self.lines.push(Line {
            spans: std::mem::take(&mut self.cur),
        });
        self.col = 0;
    }

    fn ends_space(&self) -> bool {
        self.cur
            .last()
            .is_some_and(|piece| piece.text.ends_with(' '))
    }
}

fn collapse_ws(text: &str) -> String {
    let mut out = String::new();
    let mut pending = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending = true;
            continue;
        }
        if pending {
            out.push(' ');
        }
        pending = false;
        out.push(ch);
    }
    if pending {
        out.push(' ');
    }
    out
}

pub fn line_text(line: &Line) -> String {
    line.spans.iter().map(|piece| piece.text.as_str()).collect()
}

#[cfg(test)]
pub fn line_width(line: &Line) -> usize {
    line.spans.iter().map(|piece| piece.text.width()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(html: &str, width: usize) -> Document {
        let parsed = Html::parse_document(html);
        layout(
            &parsed,
            "https://example.com/dir/page",
            width,
            &HashSet::new(),
        )
        .0
    }

    #[test]
    fn headings_links_and_wrapping() {
        let document = doc(
            "<title>Hi</title><h1>Hello</h1><p>A <a href=\"/x\">link</a> here</p><p>one two three four five</p>",
            10,
        );
        assert_eq!(document.title, "Hi");
        assert!(document.lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|piece| piece.text.contains("Hello") && piece.style == StyleKind::Heading)
        }));
        match &document.focus[0] {
            Focus::Link { url } => assert_eq!(url, "https://example.com/x"),
            other => panic!("{other:?}"),
        }
        assert!(
            document.lines.iter().all(|line| line_width(line) <= 10),
            "{:?}",
            document.lines.iter().map(line_text).collect::<Vec<_>>()
        );
        assert!(document.text_chars > 0);
    }

    #[test]
    fn drops_script_text_and_keeps_image_alt() {
        let document = doc(
            "<script>secret()</script><img alt=\"cat\" src=\"pic.png\"><p>visible</p>",
            40,
        );
        let text = document.lines.iter().map(line_text).collect::<String>();
        assert!(!text.contains("secret"));
        assert!(text.contains("cat"));
        assert!(text.contains("visible"));
        match &document.focus[0] {
            Focus::Link { url } => assert_eq!(url, "https://example.com/dir/pic.png"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn table_list_and_pre() {
        let document = doc(
            "<ul><li>one</li></ul><table><tr><td>A</td><td>B</td></tr></table><pre>a  b</pre>",
            40,
        );
        let lines: Vec<String> = document.lines.iter().map(line_text).collect();
        assert!(
            lines.iter().any(|line| line.starts_with("- one")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("A") && line.contains("B")),
            "{lines:?}"
        );
        assert!(lines.iter().any(|line| line.contains("a  b")), "{lines:?}");
    }

    #[test]
    fn empty_shell_has_no_text() {
        let document = doc("<div id=\"app\"></div>", 40);
        assert_eq!(document.text_chars, 0);
    }

    #[test]
    fn get_and_post_forms() {
        let document = doc(
            "<form action=\"/search\"><input name=\"q\" value=\"rust\"><input type=\"submit\" value=\"go\"></form><form method=\"post\" action=\"/login\"><input type=\"hidden\" name=\"t\" value=\"1\"><input name=\"user\" value=\"a\"></form>",
            80,
        );
        let get = submission(&document, 0, Some(1)).unwrap();
        assert_eq!(get.url, "https://example.com/search?q=rust");
        assert!(!get.post);
        let post = submission(&document, 1, None).unwrap();
        assert!(post.post);
        assert_eq!(post.url, "https://example.com/login");
        assert_eq!(post.body.as_deref(), Some("t=1&user=a"));
    }
}
