use std::collections::HashSet;

use scraper::{Html, Selector};

use crate::block::Blocker;
use crate::layout::{self, Document};

pub struct Rendered {
    pub document: Document,
    pub hidden: u32,
}

/// Parse `html`, drop elements Brave's cosmetic rules hide, then lay the rest out.
pub fn render(html: &str, page_url: &str, width: usize, blocker: &Blocker) -> Rendered {
    let parsed = Html::parse_document(html);
    let (classes, ids) = classes_and_ids(&parsed);
    let selectors = blocker.cosmetic_selectors(page_url, &classes, &ids);
    let mut hidden = HashSet::new();
    for raw in selectors {
        let Ok(selector) = Selector::parse(&raw) else {
            continue;
        };
        for element in parsed.select(&selector) {
            let name = element.value().name();
            if name == "html" || name == "body" {
                continue;
            }
            hidden.insert(element.id());
        }
    }
    let (document, hidden_count) = layout::layout(&parsed, page_url, width, &hidden);
    Rendered {
        document,
        hidden: hidden_count,
    }
}

fn classes_and_ids(html: &Html) -> (Vec<String>, Vec<String>) {
    let Ok(all) = Selector::parse("*") else {
        return (Vec::new(), Vec::new());
    };
    let mut classes = Vec::new();
    let mut ids = Vec::new();
    for element in html.select(&all) {
        if let Some(id) = element.value().id() {
            ids.push(id.to_string());
        }
        if let Some(class) = element.value().attr("class") {
            classes.extend(class.split_whitespace().map(str::to_string));
        }
    }
    (classes, ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::Blocker;
    use crate::layout::line_text;

    #[test]
    fn hides_the_ad_and_keeps_the_paragraph() {
        let blocker = Blocker::from_rules("example.com##.ad\n");
        let rendered = render(
            "<p class=\"ad\">buy</p><p>keep</p>",
            "https://example.com/",
            40,
            &blocker,
        );
        let text: String = rendered.document.lines.iter().map(line_text).collect();
        assert!(!text.contains("buy"), "{text}");
        assert!(text.contains("keep"), "{text}");
        assert!(rendered.hidden >= 1);
    }
}
