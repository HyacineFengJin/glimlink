// SPDX-License-Identifier: MIT
//! A bounded readable projection, not a DOM or script execution environment.
use html5gum::{Token, Tokenizer, emitters::default::DefaultEmitter};
use serde::Serialize;

pub type Result<T> = std::result::Result<T, String>;
pub const MAX_INPUT: usize = 1024 * 1024;
pub const MAX_PREVIEW: usize = 32768;
const MAX_SOURCES: usize = 128;

#[derive(Debug, Serialize)]
pub struct SourcePart {
    pub kind: String,
    pub label: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct Projection {
    pub html: String,
    pub text: String,
    pub truncated: bool,
    pub sources: Vec<SourcePart>,
    pub notes: Vec<String>,
}

pub fn escape(value: &str) -> String {
    let mut output = String::new();
    for c in value.chars() {
        match c {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            c => output.push(c),
        }
    }
    output
}

fn push_bounded(out: &mut String, value: &str, max: usize) -> bool {
    let available = max.saturating_sub(out.len());
    let mut end = available.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    out.push_str(&value[..end]);
    end < value.len()
}

fn safe_link(value: &str) -> bool {
    let value = value.trim();
    !value.chars().any(char::is_control)
        && (value.starts_with("https://")
            || value.starts_with("http://")
            || value.starts_with('/')
            || value.starts_with('#'))
}

fn block_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "aside"
            | "li"
            | "ul"
            | "ol"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "blockquote"
            | "pre"
            | "br"
            | "tr"
    )
}

pub fn normalize_html(input: &str) -> Result<Projection> {
    if input.len() > MAX_INPUT {
        return Err("HTML input exceeds 1 MiB".into());
    }
    let mut emitter = DefaultEmitter::default();
    emitter.naively_switch_states(true);
    let tokenizer = Tokenizer::new_with_emitter(input, emitter);
    let mut output = Projection {
        html: String::new(), text: String::new(), truncated: false, sources: Vec::new(),
        notes: vec!["Tokenizer projection; no DOM, CSS layout or JavaScript execution. Original source is retained separately.".into()],
    };
    let mut capture: Option<SourcePart> = None;
    let mut sources_truncated = false;
    let mut emitted = 0_usize;
    for item in tokenizer {
        let token = item.map_err(|_| "HTML input read failure")?;
        emitted += 1;
        if emitted > 65536 {
            output.truncated = true;
            break;
        }
        match token {
            Token::StartTag(tag) => {
                let name = String::from_utf8_lossy(&tag.name).into_owned();
                let attr = |key: &[u8]| -> Option<String> {
                    tag.attributes
                        .iter()
                        .find(|(k, _)| k.as_ref() == key)
                        .map(|(_, v)| String::from_utf8_lossy(&v.value).into_owned())
                };
                if matches!(name.as_str(), "script" | "style") {
                    if output.sources.len() < MAX_SOURCES {
                        if let Some(src) = attr(b"src") {
                            output.sources.push(SourcePart {
                                kind: format!("{name}_link"),
                                label: format!("external {name}"),
                                content: src,
                            });
                        }
                    } else {
                        sources_truncated = true;
                    }
                    capture = Some(SourcePart {
                        kind: name.clone(),
                        label: format!("inline {name}"),
                        content: String::new(),
                    });
                    continue;
                }
                if capture.is_some() {
                    continue;
                }
                if block_tag(&name) {
                    output.truncated |= push_bounded(&mut output.html, "\n", MAX_PREVIEW);
                    output.truncated |= push_bounded(&mut output.text, "\n", MAX_PREVIEW);
                }
                if matches!(
                    name.as_str(),
                    "a" | "img" | "audio" | "video" | "source" | "iframe" | "input" | "form"
                ) {
                    let target = attr(b"href")
                        .or_else(|| attr(b"src"))
                        .or_else(|| attr(b"data-src"))
                        .or_else(|| attr(b"action"));
                    let label = attr(b"alt")
                        .or_else(|| attr(b"title"))
                        .or_else(|| attr(b"name"))
                        .unwrap_or_default();
                    let marker = format!(
                        "[{name}{}]",
                        if label.is_empty() {
                            String::new()
                        } else {
                            format!(": {label}")
                        }
                    );
                    output.truncated |= push_bounded(&mut output.text, &marker, MAX_PREVIEW);
                    let media = matches!(
                        name.as_str(),
                        "img" | "audio" | "video" | "source" | "iframe"
                    );
                    if media {
                        if let Some(target) = target.as_ref() {
                            if output.sources.len() < MAX_SOURCES {
                                output.sources.push(SourcePart {
                                    kind: "media_link".into(),
                                    label: format!("{name} resource location"),
                                    content: target.clone(),
                                });
                            } else {
                                sources_truncated = true;
                            }
                        }
                    }
                    let html = match target.as_deref().filter(|value| !media && safe_link(value)) {
                        Some(target) => {
                            format!("<a href=\"{}\">{}</a>", escape(target), escape(&marker))
                        }
                        None => escape(&marker),
                    };
                    if output.html.len().saturating_add(html.len()) <= MAX_PREVIEW {
                        output.html.push_str(&html);
                    } else {
                        output.truncated = true;
                    }
                }
            }
            Token::EndTag(tag) => {
                let name = String::from_utf8_lossy(&tag.name);
                if capture.as_ref().is_some_and(|part| part.kind == name) {
                    let part = capture.take().unwrap();
                    if output.sources.len() < MAX_SOURCES {
                        output.sources.push(part);
                    } else {
                        sources_truncated = true;
                    }
                } else if capture.is_none() && block_tag(&name) {
                    output.truncated |= push_bounded(&mut output.html, "\n", MAX_PREVIEW);
                    output.truncated |= push_bounded(&mut output.text, "\n", MAX_PREVIEW);
                }
            }
            Token::String(value) => {
                let text = String::from_utf8_lossy(&value.value);
                if let Some(part) = capture.as_mut() {
                    part.content.push_str(&text);
                } else {
                    // Escape individual code points so truncation cannot cut an entity in half.
                    for c in text.chars() {
                        let mut bytes = [0_u8; 4];
                        let original = c.encode_utf8(&mut bytes);
                        let escaped = escape(original);
                        if output.html.len().saturating_add(escaped.len()) <= MAX_PREVIEW {
                            output.html.push_str(&escaped);
                        } else {
                            output.truncated = true;
                        }
                        output.truncated |= push_bounded(&mut output.text, original, MAX_PREVIEW);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(part) = capture {
        if output.sources.len() < MAX_SOURCES {
            output.sources.push(part);
        } else {
            sources_truncated = true;
        }
    }
    if sources_truncated {
        output
            .notes
            .push("Source index truncated; the original HTML remains available.".into());
    }
    Ok(output)
}

#[derive(Debug, Serialize)]
pub struct JsonProjection {
    pub value: serde_json::Value,
    pub truncated: bool,
    pub pointer: String,
}

pub fn normalize_json(input: &[u8], pointer: &str) -> Result<JsonProjection> {
    if input.len() > MAX_INPUT || pointer.len() > 2048 {
        return Err("JSON input/pointer exceeds limit".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(input).map_err(|e| format!("invalid JSON: {e}"))?;
    let selected = if pointer.is_empty() {
        &value
    } else {
        if !pointer.starts_with('/') {
            return Err("JSON pointer must start with /".into());
        }
        value.pointer(pointer).ok_or("JSON pointer not found")?
    };
    let serialized = serde_json::to_string(selected).map_err(|e| e.to_string())?;
    if serialized.len() <= MAX_PREVIEW {
        return Ok(JsonProjection {
            value: selected.clone(),
            truncated: false,
            pointer: pointer.into(),
        });
    }
    let mut preview = String::new();
    push_bounded(&mut preview, &serialized, MAX_PREVIEW);
    Ok(JsonProjection {
        value: serde_json::json!({"serialized_preview": preview}),
        truncated: true,
        pointer: pointer.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scripts_and_styles_are_selectable_sources_not_body_text() {
        let result = normalize_html(r#"<p>喵 &amp; me</p><script type="application/json">{"x":"<b>","n":2}</script><style>p {color:red}</style><img src="javascript:alert(1)" alt="图"><a href="/thread">帖</a>"#).unwrap();
        assert!(result.text.contains("喵 & me"));
        assert!(!result.text.contains("color:red"));
        assert!(!result.html.contains("javascript:"));
        assert!(!result.html.contains("<script"));
        assert_eq!(
            result
                .sources
                .iter()
                .filter(|v| v.kind == "script" || v.kind == "style")
                .count(),
            2
        );
        assert!(result.sources[0].content.contains("<b>"));
        assert!(result.html.contains("href=\"/thread\""));
        let result =
            normalize_html("<video src=\"https://cdn.example/video?token=private\"></video>")
                .unwrap();
        assert!(!result.html.contains("token=private"));
        assert!(result.sources[0].content.contains("token=private"));
    }
    #[test]
    fn output_is_bounded_and_json_pointer_reaches_optional_state() {
        let text = "喵<&".repeat(10000);
        let result = normalize_html(&text).unwrap();
        assert!(
            result.truncated
                && result.html.len() <= MAX_PREVIEW
                && result.text.len() <= MAX_PREVIEW
        );
        let result = normalize_json(
            br#"{"state":{"media":[{"id":1}]},"unused":2}"#,
            "/state/media/0",
        )
        .unwrap();
        assert_eq!(result.value["id"], 1);
        assert!(normalize_json(b"{}", "/missing").is_err());
    }
}
