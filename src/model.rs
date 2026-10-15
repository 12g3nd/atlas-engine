use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, String>;
pub fn now() -> String {
    Utc::now().to_rfc3339()
}
pub fn id() -> String {
    Uuid::new_v4().to_string()
}
pub fn empty_body() -> Value {
    json!({"type":"doc","content":[{"type":"paragraph"}]})
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub body: Value,
    pub kind: String,
    pub entity_type: String,
    pub metadata: Value,
    pub area_ids: Vec<String>,
    pub pinned: bool,
    pub hidden: bool,
    pub rediscovery: bool,
    pub trashed: Option<String>,
    pub created: String,
    pub updated: String,
    pub revision: u64,
    pub seed_key: Option<String>,
    pub placeholder: String,
}
impl Entry {
    pub fn blank() -> Self {
        let date = now();
        Self {
            id: id(),
            title: String::new(),
            body: empty_body(),
            kind: String::new(),
            entity_type: "note".into(),
            metadata: json!({}),
            area_ids: vec![],
            pinned: false,
            hidden: false,
            rediscovery: false,
            trashed: None,
            created: date.clone(),
            updated: date,
            revision: 0,
            seed_key: None,
            placeholder: String::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Area {
    pub id: String,
    pub name: String,
    pub description: String,
    pub parent: Option<String>,
    pub overview: String,
    pub order: i64,
    pub hidden: bool,
    pub seed_key: Option<String>,
    #[serde(default)]
    pub call_number: Option<String>,
    #[serde(default)]
    pub hue: Option<String>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub pinned: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Collection {
    pub id: String,
    pub title: String,
    pub introduction: String,
    pub members: Vec<String>,
    pub seed_key: Option<String>,
    #[serde(default)]
    pub pinned: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Relation {
    pub id: String,
    pub source: String,
    pub target: String,
    pub label: String,
    pub reason: String,
    pub origin: String,
    #[serde(default)]
    pub created: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    pub id: String,
    pub entry_id: String,
    pub name: String,
    pub relative_path: String,
    pub mime: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Revision {
    pub id: String,
    pub entry_id: String,
    pub time: String,
    pub label: String,
    pub automatic: bool,
    pub entry: Entry,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Canonical {
    pub format_version: u32,
    pub schema_version: u32,
    pub library_id: String,
    pub entries: Vec<Entry>,
    pub areas: Vec<Area>,
    pub collections: Vec<Collection>,
    pub relations: Vec<Relation>,
    pub attachments: Vec<Attachment>,
    pub revisions: Vec<Revision>,
    pub kinds: Vec<String>,
    pub seed_ledger: Vec<String>,
    /// Presentation records that belong to the library (layout, shelf, flags).
    #[serde(default)]
    pub presentation: std::collections::BTreeMap<String, String>,
}
#[derive(Deserialize)]
pub struct Destination {
    pub kind: String,
    pub id: String,
}
pub fn plain(node: &Value) -> String {
    if node["type"] == "text" {
        return node["text"].as_str().unwrap_or("").into();
    }
    if node["type"] == "hardBreak" {
        return "\n".into();
    }
    let children = node["content"].as_array();
    let text = children
        .map(|a| a.iter().map(plain).collect::<String>())
        .unwrap_or_default();
    match node["type"].as_str().unwrap_or("") {
        "paragraph" | "heading" | "listItem" | "taskItem" | "blockquote" => format!("{text}\n"),
        _ => text,
    }
}
pub fn poetry(node: &Value, out: &mut Vec<String>) {
    if node["type"] == "poetry" {
        out.push(plain(node));
    } else if let Some(a) = node["content"].as_array() {
        for c in a {
            poetry(c, out);
        }
    }
}
pub fn references(node: &Value, out: &mut Vec<String>) {
    if node["type"] == "atlasLink" {
        if let Some(s) = node["attrs"]["target"].as_str() {
            out.push(s.into());
        }
    }
    if let Some(marks) = node["marks"].as_array() {
        for m in marks {
            if m["type"] == "link" {
                if let Some(href) = m["attrs"]["href"].as_str() {
                    if let Some(t) = href.strip_prefix("atlas:") {
                        out.push(t.into());
                    }
                }
            }
        }
    }
    if let Some(a) = node["content"].as_array() {
        for c in a {
            references(c, out);
        }
    }
}
pub fn validate_body(node: &Value, depth: usize) -> Result<()> {
    if depth > 80 {
        return Err("Document nesting exceeds the supported limit".into());
    }
    let kind = node["type"].as_str().ok_or("Document node has no type")?;
    if !node.is_object()
        || (depth == 0 && kind != "doc")
        || node.get("content").map(|v| !v.is_array()).unwrap_or(false)
        || node.get("attrs").map(|v| !v.is_object()).unwrap_or(false)
        || node.get("marks").map(|v| !v.is_array()).unwrap_or(false)
    {
        return Err("Malformed document structure".into());
    }
    if ![
        "doc",
        "paragraph",
        "text",
        "hardBreak",
        "heading",
        "bulletList",
        "orderedList",
        "listItem",
        "taskList",
        "taskItem",
        "blockquote",
        "horizontalRule",
        "poetry",
        "codeBlock",
        "table",
        "tableRow",
        "tableCell",
        "tableHeader",
        "image",
        "atlasImage",
        "atlasLink",
        "attachment",
        "callout",
    ]
    .contains(&kind)
    {
        return Err(format!("Unsupported document node: {kind}"));
    }
    if kind == "image" {
        return Err("Images must reference a managed local attachment".into());
    }
    if let Some(attrs) = node["attrs"].as_object() {
        let allowed: &[&str] = match kind {
            "heading" => &["level"],
            "orderedList" => &["start", "type"],
            "taskItem" => &["checked"],
            "codeBlock" | "poetry" => &["language"],
            "tableCell" | "tableHeader" => &["colspan", "rowspan", "colwidth", "align"],
            "atlasLink" => &["target", "label", "autoLabel"],
            "atlasImage" => &["attachmentId", "alt", "caption"],
            "attachment" => &["attachmentId", "name"],
            "callout" => &["tone"],
            _ => &[],
        };
        if attrs.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err("Unsupported document attribute".into());
        }
        for key in ["label", "alt", "caption", "name", "target", "attachmentId"] {
            if attrs.get(key).map(|v| !v.is_string()).unwrap_or(false) {
                return Err("Malformed text or identity attribute".into());
            }
        }
        if attrs
            .get("checked")
            .map(|v| !v.is_boolean())
            .unwrap_or(false)
        {
            return Err("Invalid checklist state".into());
        }
        if kind == "callout"
            && attrs
                .get("tone")
                .map(|v| !v.as_str().map(|t| TONES.contains(&t)).unwrap_or(false))
                .unwrap_or(false)
        {
            return Err("Unsupported callout tone".into());
        }
        for key in ["language", "type", "align"] {
            if attrs
                .get(key)
                .map(|v| !v.is_null() && !v.is_string())
                .unwrap_or(false)
            {
                return Err("Invalid document attribute type".into());
            }
        }
        if attrs
            .get("start")
            .map(|v| {
                v.as_i64()
                    .map(|n| !(-1_000_000_000..=1_000_000_000).contains(&n))
                    .unwrap_or(true)
            })
            .unwrap_or(false)
        {
            return Err("Invalid ordered list start".into());
        }
        for key in ["colspan", "rowspan"] {
            if attrs
                .get(key)
                .map(|v| {
                    v.as_u64()
                        .map(|n| !(1..=10000).contains(&n))
                        .unwrap_or(true)
                })
                .unwrap_or(false)
            {
                return Err("Invalid table dimensions".into());
            }
        }
        if attrs
            .get("colwidth")
            .map(|v| {
                !v.is_null()
                    && v.as_array()
                        .map(|a| {
                            a.iter()
                                .any(|n| n.as_u64().map(|n| n > 100000).unwrap_or(true))
                        })
                        .unwrap_or(true)
            })
            .unwrap_or(false)
        {
            return Err("Invalid table column widths".into());
        }
    }
    if let Some(marks) = node["marks"].as_array() {
        if !marks.is_empty() && !["text", "hardBreak", "atlasLink"].contains(&kind) {
            return Err("Formatting marks must belong to inline content".into());
        }
        let mut seen = std::collections::HashSet::new();
        for m in marks {
            let name = m["type"].as_str().ok_or("Formatting mark has no type")?;
            if ![
                "bold",
                "italic",
                "strike",
                "code",
                "underline",
                "link",
                "highlight",
            ]
            .contains(&name)
                || !seen.insert(name)
                || m.get("attrs").map(|v| !v.is_object()).unwrap_or(false)
            {
                return Err("Unsupported or malformed formatting mark".into());
            }
            if let Some(attrs) = m["attrs"].as_object() {
                let allowed: &[&str] = if name == "link" {
                    &["href", "target", "rel", "class", "title"]
                } else {
                    &[]
                };
                if attrs.iter().any(|(k, v)| {
                    !allowed.contains(&k.as_str()) || (!v.is_string() && !v.is_null())
                }) {
                    return Err("Malformed formatting attributes".into());
                }
            }
            if m["type"] == "link" {
                let h = m["attrs"]["href"].as_str().unwrap_or("");
                if !safe_url(h) {
                    return Err("Unsupported or unsafe link scheme".into());
                }
            }
        }
    }
    let children = node["content"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let block = [
        "paragraph",
        "heading",
        "bulletList",
        "orderedList",
        "taskList",
        "blockquote",
        "horizontalRule",
        "poetry",
        "codeBlock",
        "table",
        "atlasImage",
        "attachment",
        "callout",
    ];
    let inline = ["text", "hardBreak", "atlasLink"];
    let allowed: &[&str] = match kind {
        "doc" | "blockquote" | "callout" | "tableCell" | "tableHeader" => &block,
        "paragraph" | "heading" => &inline,
        "poetry" | "codeBlock" => &["text"],
        "bulletList" | "orderedList" => &["listItem"],
        "taskList" => &["taskItem"],
        "listItem" | "taskItem" => &block,
        "table" => &["tableRow"],
        "tableRow" => &["tableCell", "tableHeader"],
        _ => &[],
    };
    if children
        .iter()
        .any(|c| !allowed.contains(&c["type"].as_str().unwrap_or("")))
    {
        return Err("Document node contains invalid children".into());
    }
    if [
        "doc",
        "blockquote",
        "callout",
        "listItem",
        "taskItem",
        "bulletList",
        "orderedList",
        "taskList",
        "table",
        "tableRow",
        "tableCell",
        "tableHeader",
    ]
    .contains(&kind)
        && children.is_empty()
    {
        return Err("Required document content is missing".into());
    }
    if ["listItem", "taskItem"].contains(&kind)
        && children
            .first()
            .map(|c| c["type"] != "paragraph")
            .unwrap_or(true)
    {
        return Err("A list item must begin with a paragraph".into());
    }
    if kind == "text" && node["text"].as_str().map(|s| s.is_empty()).unwrap_or(true) {
        return Err("Text nodes require non-empty text".into());
    }
    if kind == "heading"
        && node
            .get("attrs")
            .and_then(|a| a.get("level"))
            .map(|v| v.as_u64().map(|n| !(1..=6).contains(&n)).unwrap_or(true))
            .unwrap_or(false)
    {
        return Err("Invalid heading level".into());
    }
    if ["atlasImage", "attachment"].contains(&kind)
        && uuid::Uuid::parse_str(node["attrs"]["attachmentId"].as_str().unwrap_or("")).is_err()
    {
        return Err("Invalid attachment identity".into());
    }
    if kind == "atlasLink"
        && (uuid::Uuid::parse_str(node["attrs"]["target"].as_str().unwrap_or("")).is_err()
            || node["attrs"]
                .get("autoLabel")
                .map(|v| !v.is_boolean())
                .unwrap_or(false)
            || node["attrs"]
                .get("label")
                .map(|v| !v.is_string())
                .unwrap_or(false))
    {
        return Err("Invalid stable page link".into());
    }
    if ["poetry", "codeBlock"].contains(&kind)
        && children.iter().any(|c| {
            c["marks"]
                .as_array()
                .map(|m| !m.is_empty())
                .unwrap_or(false)
        })
    {
        return Err("Poetry and code blocks cannot contain formatting marks".into());
    }
    if let Some(a) = node["content"].as_array() {
        for c in a {
            validate_body(c, depth + 1)?;
        }
    }
    Ok(())
}
pub const TONES: [&str; 7] = ["lilac", "butter", "peach", "rose", "sage", "sky", "mint"];
pub fn safe_url(h: &str) -> bool {
    if h.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    if let Some(target) = h.strip_prefix("atlas:") {
        return uuid::Uuid::parse_str(target).is_ok();
    }
    url::Url::parse(h)
        .map(|url| match url.scheme() {
            "http" | "https" => url.host_str().is_some(),
            "mailto" => !url.path().is_empty(),
            _ => false,
        })
        .unwrap_or(false)
}

pub fn markdown(node: &Value) -> String {
    let kind = node["type"].as_str().unwrap_or("");
    let child = || {
        node["content"]
            .as_array()
            .map(|a| a.iter().map(markdown).collect::<String>())
            .unwrap_or_default()
    };
    match kind {
        "text" => {
            let mut s = node["text"]
                .as_str()
                .unwrap_or("")
                .chars()
                .flat_map(|c| {
                    if ['\\', '*', '_', '[', ']', '<', '>', '`', '~'].contains(&c) {
                        vec!['\\', c]
                    } else {
                        vec![c]
                    }
                })
                .collect::<String>();
            if let Some(m) = node["marks"].as_array() {
                for mark in m {
                    match mark["type"].as_str().unwrap_or("") {
                        "bold" => s = format!("**{s}**"),
                        "italic" => s = format!("*{s}*"),
                        "strike" => s = format!("~~{s}~~"),
                        "code" => s = format!("`{s}`"),
                        "highlight" => s = format!("=={s}=="),
                        "link" => {
                            s = format!("[{s}]({})", mark["attrs"]["href"].as_str().unwrap_or(""))
                        }
                        _ => (),
                    }
                }
            }
            s
        }
        "paragraph" => format!(
            "{}\n\n",
            child()
                .split('\n')
                .map(guard)
                .collect::<Vec<_>>()
                .join("\n")
        ),
        "heading" => format!(
            "{} {}\n\n",
            "#".repeat(node["attrs"]["level"].as_u64().unwrap_or(2).min(6) as usize),
            child()
        ),
        "hardBreak" => "  \n".into(),
        "horizontalRule" => "\n---\n\n".into(),
        "poetry" | "codeBlock" => {
            let s = plain(node);
            let mut fence = "```".to_string();
            while s.contains(&fence) {
                fence.push('`');
            }
            format!("{fence}poem\n{s}\n{fence}\n\n")
        }
        "blockquote" => format!(
            "{}\n",
            child()
                .trim_end()
                .split('\n')
                .map(|l| format!("> {l}\n"))
                .collect::<String>()
        ),
        "callout" => format!(
            "> [!{}]\n{}\n\n",
            node["attrs"]["tone"].as_str().unwrap_or("butter"),
            child()
                .trim_end()
                .lines()
                .map(|l| format!("> {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        "bulletList" | "taskList" => format!("{}\n", list_items(node, |_| "- ".into())),
        "orderedList" => {
            let start = node["attrs"]["start"].as_i64().unwrap_or(1);
            format!(
                "{}\n",
                list_items(node, |i| format!("{}. ", start + i as i64))
            )
        }
        "atlasLink" => format!(
            "[{}]({}.md)",
            node["attrs"]["label"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("Linked page"),
            node["attrs"]["target"].as_str().unwrap_or("")
        ),
        "atlasImage" | "attachment" => format!(
            "[{}](../attachments/{})\n",
            node["attrs"]["alt"].as_str().unwrap_or("Local attachment"),
            node["attrs"]["attachmentId"].as_str().unwrap_or("")
        ),
        // GFM needs the separator row; the first row is its header.
        "table" => {
            let rows: Vec<String> = node["content"]
                .as_array()
                .map(|r| r.iter().map(markdown).collect())
                .unwrap_or_default();
            let columns = node["content"][0]["content"].as_array().map_or(1, Vec::len);
            match rows.split_first() {
                Some((head, body)) => {
                    format!("{head}|{}\n{}\n", " --- |".repeat(columns), body.concat())
                }
                None => String::new(),
            }
        }
        "tableRow" => format!("|{}\n", child()),
        "tableCell" | "tableHeader" => format!(
            " {} |",
            child()
                .trim()
                .replace("\n\n", " ")
                .replace('\n', " ")
                .replace('|', "\\|")
        ),
        _ => child(),
    }
}

// List items carry their marker on the first line and indent the rest under
// it, so nested lists and second paragraphs stay inside the item.
fn list_items(node: &Value, marker: impl Fn(usize) -> String) -> String {
    let Some(items) = node["content"].as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        let mark = marker(i);
        let pad = " ".repeat(mark.len());
        let task = match (item["type"].as_str(), item["attrs"]["checked"].as_bool()) {
            (Some("taskItem"), Some(true)) => "[x] ",
            (Some("taskItem"), _) => "[ ] ",
            _ => "",
        };
        let body = item["content"]
            .as_array()
            .map(|a| a.iter().map(markdown).collect::<String>())
            .unwrap_or_default();
        let lines: Vec<&str> = body.trim_end().split('\n').collect();
        for (j, line) in lines.iter().enumerate() {
            // A nested list follows its paragraph directly, keeping the list tight.
            let nested = |l: &&str| {
                let digits = l.chars().take_while(char::is_ascii_digit).count();
                l.starts_with("- ") || (digits > 0 && l[digits..].starts_with(". "))
            };
            if line.is_empty() && lines.get(j + 1).is_some_and(nested) {
                continue;
            }
            if j == 0 && line.is_empty() {
                out.push_str(format!("{mark}{task}").trim_end());
            } else if j == 0 {
                out.push_str(&format!("{mark}{task}{line}"));
            } else if !line.is_empty() {
                out.push_str(&pad);
                out.push_str(line);
            }
            out.push('\n');
        }
    }
    out
}

// Text that would read as block syntax at the start of a line is escaped, so
// a paragraph beginning "# " or "1. " comes back as a paragraph.
fn guard(line: &str) -> String {
    let rest = |n: usize| line[n..].chars().next().is_none_or(char::is_whitespace);
    let hashes = line.chars().take_while(|c| *c == '#').count();
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    let risky = (hashes > 0 && rest(hashes))
        || ((line.starts_with('-') || line.starts_with('+')) && rest(1))
        || (!line.is_empty() && line.chars().all(|c| c == '=' || c == '-'))
        || ((1..=9).contains(&digits)
            && line[digits..].starts_with(['.', ')'])
            && rest(digits + 1));
    if risky {
        format!("\\{line}")
    } else {
        line.to_string()
    }
}

// Imported HTML is inert text. Unsupported Markdown is reported by the caller.
// Highlights (`==text==`), `> [!tone]` callouts, tables, task lists and
// strikethrough come back as the editor's own; front matter stays as text.
pub fn from_markdown(source: &str) -> Value {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let block = |kind: &str| json!({"type": kind, "content": []});
    let mut stack: Vec<Value> = vec![block("doc")];
    let mut marks: Vec<Value> = vec![];
    let mut in_head = false;
    for event in Parser::new_ext(source, options) {
        match event {
            Event::Start(tag) => {
                match tag {
                    Tag::Emphasis => marks.push(json!({"type":"italic"})),
                    Tag::Strong => marks.push(json!({"type":"bold"})),
                    Tag::Strikethrough => marks.push(json!({"type":"strike"})),
                    Tag::Link { dest_url, .. } => marks.push(if safe_url(&dest_url) {
                        json!({"type":"link","attrs":{"href":dest_url.to_string()}})
                    } else {
                        json!({"type":"italic"})
                    }),
                    Tag::Paragraph | Tag::HtmlBlock => stack.push(block("paragraph")),
                    Tag::Heading { level, .. } => stack
                        .push(json!({"type":"heading","attrs":{"level":level as u8},"content":[]})),
                    Tag::BlockQuote(_) => stack.push(block("blockquote")),
                    Tag::List(Some(start)) if start != 1 => stack
                        .push(json!({"type":"orderedList","attrs":{"start":start},"content":[]})),
                    Tag::List(start) => stack.push(block(if start.is_some() {
                        "orderedList"
                    } else {
                        "bulletList"
                    })),
                    Tag::Item => stack.push(block("listItem")),
                    Tag::CodeBlock(CodeBlockKind::Fenced(lang)) if lang.as_ref() == "poem" => {
                        stack.push(block("poetry"))
                    }
                    Tag::CodeBlock(_) => stack.push(block("codeBlock")),
                    Tag::MetadataBlock(_) => stack
                        .push(json!({"type":"codeBlock","attrs":{"language":"yaml"},"content":[]})),
                    Tag::Table(_) => stack.push(block("table")),
                    Tag::TableHead => {
                        in_head = true;
                        stack.push(block("tableRow"))
                    }
                    Tag::TableRow => stack.push(block("tableRow")),
                    Tag::TableCell => {
                        stack.push(block(if in_head { "tableHeader" } else { "tableCell" }))
                    }
                    _ => (),
                }
            }
            Event::End(tag) => match tag {
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                    marks.pop();
                }
                TagEnd::TableHead => {
                    in_head = false;
                    close(&mut stack)
                }
                TagEnd::Paragraph
                | TagEnd::HtmlBlock
                | TagEnd::Heading(_)
                | TagEnd::BlockQuote(_)
                | TagEnd::List(_)
                | TagEnd::Item
                | TagEnd::CodeBlock
                | TagEnd::MetadataBlock(_)
                | TagEnd::Table
                | TagEnd::TableRow
                | TagEnd::TableCell => close(&mut stack),
                _ => (),
            },
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => {
                // Nested emphasis, or an unsafe link inside italics, would
                // repeat a mark, which the schema refuses.
                let mut kept: Vec<Value> = vec![];
                for m in &marks {
                    if !kept.iter().any(|k| k["type"] == m["type"]) {
                        kept.push(m.clone());
                    }
                }
                let mut n = json!({"type":"text","text":t.to_string()});
                if !kept.is_empty() {
                    n["marks"] = json!(kept);
                }
                inline(&mut stack, n)
            }
            Event::Code(t) => inline(
                &mut stack,
                json!({"type":"text","text":t.to_string(),"marks":[{"type":"code"}]}),
            ),
            Event::SoftBreak => inline(&mut stack, json!({"type":"text","text":"\n"})),
            Event::HardBreak => inline(&mut stack, json!({"type":"hardBreak"})),
            Event::Rule => add(&mut stack, json!({"type":"horizontalRule"})),
            Event::TaskListMarker(checked) => {
                if let Some(item) = stack.iter_mut().rev().find(|v| v["type"] == "listItem") {
                    item["type"] = json!("taskItem");
                    item["attrs"] = json!({ "checked": checked });
                }
            }
            _ => (),
        }
    }
    while stack.len() > 1 {
        close(&mut stack);
    }
    let mut doc = stack.pop().unwrap();
    if doc["content"].as_array().unwrap().is_empty() {
        doc = empty_body();
    }
    doc
}

const INLINE: [&str; 3] = ["text", "hardBreak", "atlasLink"];

fn add(stack: &mut [Value], v: Value) {
    stack.last_mut().unwrap()["content"]
        .as_array_mut()
        .unwrap()
        .push(v);
}

fn inline(stack: &mut Vec<Value>, n: Value) {
    if n["type"] == "text" && n["text"].as_str().is_none_or(str::is_empty) {
        return;
    }
    let top = &stack.last().unwrap()["type"];
    if top == "doc" || top == "blockquote" {
        stack.push(json!({"type":"paragraph","content":[]}));
        add(stack, n);
        close(stack);
    } else {
        add(stack, n);
    }
}

fn close(stack: &mut Vec<Value>) {
    if stack.len() < 2 {
        return;
    }
    let mut v = stack.pop().unwrap();
    let content = v["content"].as_array().cloned().unwrap_or_default();
    match v["type"].as_str().unwrap_or("") {
        "paragraph" | "heading" => v["content"] = json!(highlights(content)),
        // Tight list items and table cells hold inline content directly.
        // Give it the paragraphs the editor's schema expects.
        "listItem" | "taskItem" | "tableCell" | "tableHeader" => {
            let mut blocks = Vec::new();
            let mut run = Vec::new();
            for c in content {
                if INLINE.contains(&c["type"].as_str().unwrap_or("")) {
                    run.push(c);
                } else {
                    if !run.is_empty() {
                        blocks.push(
                            json!({"type":"paragraph","content":highlights(std::mem::take(&mut run))}),
                        );
                    }
                    blocks.push(c);
                }
            }
            if !run.is_empty() {
                blocks.push(json!({"type":"paragraph","content":highlights(run)}));
            }
            if blocks.first().is_none_or(|b| b["type"] != "paragraph") {
                blocks.insert(0, json!({"type":"paragraph","content":[]}));
            }
            v["content"] = json!(blocks);
        }
        "bulletList" | "orderedList" if content.iter().any(|c| c["type"] == "taskItem") => {
            let items: Vec<Value> = content
                .into_iter()
                .map(|mut c| {
                    if c["type"] == "listItem" {
                        c["type"] = json!("taskItem");
                        c["attrs"] = json!({ "checked": false });
                    }
                    c
                })
                .collect();
            v = json!({"type":"taskList","content":items});
        }
        "blockquote" => {
            callout(&mut v);
            if v["content"].as_array().is_none_or(Vec::is_empty) {
                v["content"] = json!([{"type":"paragraph","content":[]}]);
            }
        }
        _ => (),
    }
    add(stack, v);
}

// `> [!tone]` is the export's callout. GitHub and Obsidian callout names map
// onto the nearest tone, and an Obsidian title stays as the first line.
fn callout(v: &mut Value) {
    let first = &v["content"][0];
    if first["type"] != "paragraph" || first["content"][0].get("marks").is_some() {
        return;
    }
    let Some(text) = first["content"][0]["text"].as_str() else {
        return;
    };
    let Some((name, after)) = text
        .strip_prefix("[!")
        .and_then(|rest| rest.split_once(']'))
    else {
        return;
    };
    let name = name.to_lowercase();
    let tone: &str = match name.as_str() {
        t if TONES.contains(&t) => TONES.iter().find(|x| **x == t).unwrap(),
        "note" | "info" | "abstract" | "summary" => "sky",
        "tip" | "hint" | "success" | "check" | "done" => "sage",
        "important" | "question" | "help" | "faq" => "lilac",
        "warning" | "attention" | "todo" => "butter",
        "caution" | "danger" | "error" | "failure" | "fail" | "bug" => "rose",
        "example" | "quote" | "cite" => "peach",
        _ => return,
    };
    let after = after.trim_start_matches(['-', '+']);
    let after = after
        .strip_prefix('\n')
        .unwrap_or(after.strip_prefix(' ').unwrap_or(after))
        .to_string();
    let paragraph = &mut v["content"][0]["content"];
    if after.is_empty() {
        paragraph.as_array_mut().unwrap().remove(0);
    } else {
        paragraph[0]["text"] = json!(after);
    }
    if paragraph.as_array().is_some_and(Vec::is_empty) {
        v["content"].as_array_mut().unwrap().remove(0);
    }
    v["type"] = json!("callout");
    v["attrs"] = json!({ "tone": tone });
}

// `==text==` is the export's highlight, and Obsidian's. Delimiters pair the
// way emphasis does: an opener is followed by a letter, a closer follows one,
// so "a == b" stays as typed.
fn highlights(nodes: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = vec![];
    for n in nodes {
        if let Some(last) = merged.last_mut() {
            if n["type"] == "text" && last["type"] == "text" && last.get("marks") == n.get("marks")
            {
                let joined = format!(
                    "{}{}",
                    last["text"].as_str().unwrap_or(""),
                    n["text"].as_str().unwrap_or("")
                );
                last["text"] = json!(joined);
                continue;
            }
        }
        merged.push(n);
    }
    let code = |n: &Value| {
        n["marks"]
            .as_array()
            .is_some_and(|m| m.iter().any(|m| m["type"] == "code"))
    };
    let mut flat = String::new();
    let mut starts = vec![];
    for n in &merged {
        starts.push(flat.len());
        flat.push_str(n["text"].as_str().unwrap_or(" "));
    }
    let solid = |c: Option<char>| c.is_some_and(|c| !c.is_whitespace() && c != '=');
    let mut spans: Vec<(usize, usize)> = vec![];
    let mut open: Option<usize> = None;
    for (i, n) in merged.iter().enumerate() {
        if n["type"] != "text" || code(n) {
            continue;
        }
        for (o, _) in n["text"].as_str().unwrap_or("").match_indices("==") {
            let at = starts[i] + o;
            match open {
                Some(from) if solid(flat[..at].chars().next_back()) => {
                    spans.push((from, at));
                    open = None;
                }
                None if solid(flat[at + 2..].chars().next()) => open = Some(at),
                _ => (),
            }
        }
    }
    if spans.is_empty() {
        return merged;
    }
    let cut = |at: usize| spans.iter().any(|&(a, b)| at == a || at == b);
    let lit = |at: usize| spans.iter().any(|&(a, b)| at >= a + 2 && at < b);
    let mut out = vec![];
    for (i, n) in merged.into_iter().enumerate() {
        if n["type"] != "text" || code(&n) {
            out.push(n);
            continue;
        }
        let text = n["text"].as_str().unwrap_or("").to_string();
        let mut pieces = vec![];
        let mut from = 0;
        for (o, _) in text.match_indices("==") {
            if cut(starts[i] + o) {
                pieces.push((from, o));
                from = o + 2;
            }
        }
        pieces.push((from, text.len()));
        for (a, b) in pieces {
            if a == b {
                continue;
            }
            let mut piece = n.clone();
            piece["text"] = json!(&text[a..b]);
            if lit(starts[i] + a) {
                let mut m = piece["marks"].as_array().cloned().unwrap_or_default();
                m.push(json!({"type":"highlight"}));
                piece["marks"] = json!(m);
            }
            out.push(piece);
        }
    }
    out
}
