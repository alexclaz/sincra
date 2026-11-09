//! Minimal mutable XML DOM (enough for FCP7 xmeml), with an ElementTree-like tab-indented writer.

use anyhow::{bail, Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::tr;

#[derive(Clone, Debug)]
pub enum Node {
    Element(Element),
    Text(String),
}

#[derive(Clone, Debug, Default)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Element {
    pub fn new(name: &str) -> Self {
        Element { name: name.to_string(), ..Default::default() }
    }

    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            _ => None,
        })
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.elements().filter(move |e| e.name == name)
    }

    pub fn child(&self, name: &str) -> Option<&Element> {
        self.elements().find(|e| e.name == name)
    }

    pub fn child_mut(&mut self, name: &str) -> Option<&mut Element> {
        self.children.iter_mut().find_map(|n| match n {
            Node::Element(e) if e.name == name => Some(e),
            _ => None,
        })
    }

    /// `a/b/c` path of direct children (first match at every level), like ElementTree.find.
    pub fn find(&self, path: &str) -> Option<&Element> {
        let mut cur = self;
        for part in path.split('/') {
            cur = cur.child(part)?;
        }
        Some(cur)
    }

    pub fn find_mut(&mut self, path: &str) -> Option<&mut Element> {
        let mut cur = self;
        for part in path.split('/') {
            cur = cur.child_mut(part)?;
        }
        Some(cur)
    }

    /// Concatenated direct text of the element ("" when it has none).
    pub fn text(&self) -> String {
        let mut s = String::new();
        for n in &self.children {
            if let Node::Text(t) = n {
                s.push_str(t);
            }
        }
        s
    }

    /// ElementTree.findtext: None when the element is missing, "" when it has no text.
    pub fn findtext(&self, path: &str) -> Option<String> {
        self.find(path).map(|e| e.text())
    }

    /// findtext that treats empty text like a missing element (Python's `findtext(...) or ...`).
    pub fn findtext_nonempty(&self, path: &str) -> Option<String> {
        self.findtext(path).filter(|s| !s.is_empty())
    }

    pub fn set_text(&mut self, text: &str) {
        self.children.retain(|n| !matches!(n, Node::Text(_)));
        self.children.insert(0, Node::Text(text.to_string()));
    }

    /// Set the text of direct child `tag`, creating it at the end when missing.
    pub fn set_child_text(&mut self, tag: &str, value: &str) {
        if let Some(c) = self.child_mut(tag) {
            c.set_text(value);
        } else {
            let mut c = Element::new(tag);
            c.set_text(value);
            self.children.push(Node::Element(c));
        }
    }

    /// Remove the first direct child named `tag`.
    pub fn remove_child(&mut self, tag: &str) -> Option<Element> {
        let i = self.children.iter().position(|n| matches!(n, Node::Element(e) if e.name == tag))?;
        match self.children.remove(i) {
            Node::Element(e) => Some(e),
            _ => None,
        }
    }

    /// Pre-order paths (child indices from `self`) of all descendants named `name`, self included.
    pub fn iter_paths(&self, name: &str) -> Vec<Vec<usize>> {
        fn walk(e: &Element, name: &str, path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
            if e.name == name {
                out.push(path.clone());
            }
            for (i, n) in e.children.iter().enumerate() {
                if let Node::Element(c) = n {
                    path.push(i);
                    walk(c, name, path, out);
                    path.pop();
                }
            }
        }
        let mut out = Vec::new();
        walk(self, name, &mut Vec::new(), &mut out);
        out
    }

    /// Pre-order iteration over all descendants named `name`, self included.
    pub fn iter_named<'a>(&'a self, name: &'a str, out: &mut Vec<&'a Element>) {
        if self.name == name {
            out.push(self);
        }
        for c in self.elements() {
            c.iter_named(name, out);
        }
    }

    pub fn at_path(&self, path: &[usize]) -> Option<&Element> {
        let mut cur = self;
        for &i in path {
            cur = match cur.children.get(i)? {
                Node::Element(e) => e,
                _ => return None,
            };
        }
        Some(cur)
    }

    pub fn at_path_mut(&mut self, path: &[usize]) -> Option<&mut Element> {
        let mut cur = self;
        for &i in path {
            cur = match cur.children.get_mut(i)? {
                Node::Element(e) => e,
                _ => return None,
            };
        }
        Some(cur)
    }
}

pub fn parse(bytes: &[u8]) -> Result<Element> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut buf = Vec::new();

    fn start(e: &quick_xml::events::BytesStart) -> Result<Element> {
        let mut el = Element::new(&String::from_utf8_lossy(e.name().as_ref()));
        for a in e.attributes().with_checks(false) {
            let a = a?;
            let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
            let val = a.unescape_value().map(|v| v.into_owned())
                .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned());
            el.attrs.push((key, val));
        }
        Ok(el)
    }

    loop {
        let ev = reader.read_event_into(&mut buf).context(tr!("XML mal formado", "Malformed XML"))?;
        match ev {
            Event::Start(e) => stack.push(start(&e)?),
            Event::Empty(e) => {
                let el = start(&e)?;
                match stack.last_mut() {
                    Some(p) => p.children.push(Node::Element(el)),
                    None => root = Some(el),
                }
            }
            Event::End(_) => {
                let el = stack.pop().context(tr!("XML mal formado: tag de fechamento sobrando", "Malformed XML: unexpected closing tag"))?;
                match stack.last_mut() {
                    Some(p) => p.children.push(Node::Element(el)),
                    None => root = Some(el),
                }
            }
            Event::Text(t) => {
                if let Some(p) = stack.last_mut() {
                    let s = t.unescape().map(|v| v.into_owned())
                        .unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned());
                    push_text(p, s);
                }
            }
            Event::CData(t) => {
                if let Some(p) = stack.last_mut() {
                    push_text(p, String::from_utf8_lossy(&t).into_owned());
                }
            }
            Event::Eof => break,
            _ => {} // declaration, doctype, comments and PIs are dropped (as ElementTree does)
        }
        buf.clear();
    }
    if !stack.is_empty() {
        bail!(tr!("XML mal formado: tags sem fechamento", "Malformed XML: unclosed tags"));
    }
    root.context(tr!("XML vazio", "Empty XML"))
}

fn push_text(p: &mut Element, s: String) {
    if let Some(Node::Text(prev)) = p.children.last_mut() {
        prev.push_str(&s);
    } else {
        p.children.push(Node::Text(s));
    }
}

fn esc_text(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

fn esc_attr(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#09;"),
            c => out.push(c),
        }
    }
}

/// Serialize like `ET.indent(tree, space="\t")` + `tree.write()`: elements that contain elements are
/// indented with tabs (whitespace-only text dropped), leaf elements keep their text verbatim.
pub fn write(root: &Element) -> String {
    let mut out = String::new();
    write_el(root, 0, &mut out);
    out
}

fn write_el(e: &Element, level: usize, out: &mut String) {
    out.push('<');
    out.push_str(&e.name);
    for (k, v) in &e.attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        esc_attr(v, out);
        out.push('"');
    }
    let has_el = e.children.iter().any(|n| matches!(n, Node::Element(_)));
    if e.children.is_empty() || (!has_el && e.text().is_empty()) {
        out.push_str(" />");
        return;
    }
    out.push('>');
    if !has_el {
        esc_text(&e.text(), out);
    } else {
        for n in &e.children {
            match n {
                Node::Text(t) if t.trim().is_empty() => {}
                Node::Text(t) => esc_text(t, out),
                Node::Element(c) => {
                    out.push('\n');
                    for _ in 0..=level {
                        out.push('\t');
                    }
                    write_el(c, level + 1, out);
                }
            }
        }
        out.push('\n');
        for _ in 0..level {
            out.push('\t');
        }
    }
    out.push_str("</");
    out.push_str(&e.name);
    out.push('>');
}
