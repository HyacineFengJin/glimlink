// SPDX-License-Identifier: MIT
use crate::transport::fixed_api_uri;
use std::collections::BTreeMap;

pub(crate) const MAX_PAGE: u32 = 100_000;
pub(crate) struct Pagination {
    pub has_more: Option<bool>,
    pub next_page: Option<u32>,
    pub warning: Option<String>,
}
fn unavailable(reason: &'static str) -> Pagination {
    Pagination {
        has_more: None,
        next_page: None,
        warning: Some(format!("pagination_unavailable: {reason}")),
    }
}
pub(crate) fn next_page(
    link: Option<&str>,
    path: &str,
    query: &[(&str, String)],
    current: u32,
    per_page: u32,
) -> Pagination {
    let Some(link) = link else {
        return Pagination {
            has_more: Some(false),
            next_page: None,
            warning: None,
        };
    };
    let Ok(parts) = split_links(link) else {
        return unavailable("malformed Link header");
    };
    let mut next = None;
    for part in parts {
        let Ok((url, relations)) = parse_part(part) else {
            return unavailable("malformed Link relation");
        };
        if !relations.split_ascii_whitespace().any(|r| r == "next") {
            continue;
        }
        if next.is_some() {
            return unavailable("ambiguous next relation");
        }
        next = Some(url);
    }
    let Some(url) = next else {
        return Pagination {
            has_more: Some(false),
            next_page: None,
            warning: None,
        };
    };
    match validated_page(url, path, query, current, per_page) {
        Some(page) => Pagination {
            has_more: Some(true),
            next_page: Some(page),
            warning: None,
        },
        None => unavailable("next relation is not a bounded continuation of this resource"),
    }
}
fn split_links(header: &str) -> std::result::Result<Vec<&str>, ()> {
    if header.is_empty() || header.len() > 32 * 1024 || header.chars().any(|c| c.is_control()) {
        return Err(());
    }
    let mut out = Vec::new();
    let mut start = 0;
    let mut angle = false;
    let mut quote = false;
    let mut escaped = false;
    for (i, c) in header.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote && c == '\\' {
            escaped = true;
            continue;
        }
        match c {
            '"' if !angle => quote = !quote,
            '<' if !quote && !angle => angle = true,
            '<' if !quote => return Err(()),
            '>' if !quote && angle => angle = false,
            '>' if !quote => return Err(()),
            ',' if !angle && !quote => {
                out.push(header[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    if angle || quote || escaped {
        return Err(());
    }
    out.push(header[start..].trim());
    if out.iter().any(|s| s.is_empty()) {
        return Err(());
    }
    Ok(out)
}
fn parse_part(part: &str) -> std::result::Result<(&str, &str), ()> {
    let rest = part.strip_prefix('<').ok_or(())?;
    let (url, params) = rest.split_once('>').ok_or(())?;
    if url.is_empty() || !params.trim_start().starts_with(';') {
        return Err(());
    }
    let mut relation = None;
    for parameter in params.trim_start().strip_prefix(';').ok_or(())?.split(';') {
        let (key, value) = parameter.trim().split_once('=').ok_or(())?;
        let value = value.trim();
        if key.trim().eq_ignore_ascii_case("rel") {
            if relation.is_some() {
                return Err(());
            }
            let value = if value.starts_with('"') {
                value
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .ok_or(())?
            } else {
                value
            };
            if value.is_empty() || value.contains(['"', '\\', '<', '>']) {
                return Err(());
            }
            relation = Some(value);
        }
    }
    Ok((url, relation.ok_or(())?))
}
fn validated_page(
    url: &str,
    path: &str,
    query: &[(&str, String)],
    current: u32,
    per_page: u32,
) -> Option<u32> {
    let uri = fixed_api_uri(url).ok()?;
    if !same_resource(uri.path(), path) {
        return None;
    }
    let candidate = parse_query(uri.query()?)?;
    let expected: BTreeMap<String, String> = query
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    if candidate.len() != expected.len() || candidate.keys().ne(expected.keys()) {
        return None;
    }
    for (k, v) in &expected {
        if k != "page" && candidate.get(k) != Some(v) {
            return None;
        }
    }
    if candidate.get("per_page")?.parse::<u32>().ok() != Some(per_page) {
        return None;
    }
    let page = candidate.get("page")?;
    if page.is_empty() || !page.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let page = page.parse::<u32>().ok()?;
    if page <= current || page > MAX_PAGE {
        return None;
    }
    Some(page)
}
fn same_resource(candidate: &str, expected: &str) -> bool {
    if candidate == expected {
        return true;
    }
    // GitHub frequently emits canonical /repositories/<numeric-id>/... URLs.
    // Only their page number is used; the next request is rebuilt from the
    // original owner/repository, so this cannot switch repository identity.
    let Some(repo) = expected.strip_prefix("/repos/") else {
        return false;
    };
    let mut parts = repo.splitn(3, '/');
    if parts.next().is_none() || parts.next().is_none() {
        return false;
    }
    let Some(suffix) = parts.next() else {
        return false;
    };
    let Some(canonical) = candidate.strip_prefix("/repositories/") else {
        return false;
    };
    let Some((id, tail)) = canonical.split_once('/') else {
        return false;
    };
    !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
        && id.parse::<u64>().is_ok_and(|id| id > 0)
        && suffix == tail
}
fn parse_query(query: &str) -> Option<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        let key = decode(key)?;
        let value = decode(value)?;
        if key.is_empty() || out.insert(key, value).is_some() {
            return None;
        }
    }
    Some(out)
}
fn decode(value: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut bytes = value.bytes();
    while let Some(b) = bytes.next() {
        out.push(match b {
            b'%' => {
                let a = (bytes.next()? as char).to_digit(16)?;
                let b = (bytes.next()? as char).to_digit(16)?;
                (a * 16 + b) as u8
            }
            b'+' => b' ',
            _ => b,
        });
    }
    let value = String::from_utf8(out).ok()?;
    if value.chars().any(|c| c.is_control()) {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn query() -> Vec<(&'static str, String)> {
        vec![
            ("state", "open".into()),
            ("page", "1".into()),
            ("per_page", "20".into()),
        ]
    }
    #[test]
    fn follows_only_validated_page_hint_including_canonical_repository_paths() {
        for path in ["/repos/o/r/issues", "/repositories/123/issues"] {
            let link = format!(
                "<https://api.github.com{path}?state=open&page=2&per_page=20>; rel=\"next\", <https://api.github.com{path}?state=open&page=9&per_page=20>; rel=\"last\""
            );
            let page = next_page(Some(&link), "/repos/o/r/issues", &query(), 1, 20);
            assert_eq!(page.next_page, Some(2));
            assert_eq!(page.has_more, Some(true));
            assert!(page.warning.is_none());
        }
    }
    #[test]
    fn untrusted_or_repeating_links_preserve_page_with_explicit_unknown_tail() {
        for url in [
            "https://evil.test/repos/o/r/issues?state=open&page=2&per_page=20",
            "https://api.github.com@evil.test/repos/o/r/issues?state=open&page=2&per_page=20",
            "http://api.github.com/repos/o/r/issues?state=open&page=2&per_page=20",
            "https://api.github.com/repos/o/other/issues?state=open&page=2&per_page=20",
            "https://api.github.com/repositories/123/pulls?state=open&page=2&per_page=20",
            "https://api.github.com/repos/o/r/issues?state=open&page=1&per_page=20",
            "https://api.github.com/repos/o/r/issues?state=open&page=2&per_page=100",
            "https://api.github.com/repos/o/r/issues?state=closed&page=2&per_page=20",
            "https://api.github.com/repos/o/r/issues?state=open&page=2&per_page=20&page=3",
            "https://api.github.com/repos/o/r/issues?state=open&page=100001&per_page=20",
            "https://api.github.com/repos/o/r/issues?state=open&page=2",
            "https://api.github.com/repos/o/r/issues?state=open&page=2&per_page=20&token=bad",
        ] {
            let page = next_page(
                Some(&format!("<{url}>; rel=\"next\"")),
                "/repos/o/r/issues",
                &query(),
                1,
                20,
            );
            assert_eq!(page.has_more, None, "{url}");
            assert_eq!(page.next_page, None);
            assert!(page.warning.unwrap().starts_with("pagination_unavailable:"));
        }
    }
    #[test]
    fn end_of_collection_differs_from_malformed_or_ambiguous_header() {
        assert_eq!(
            next_page(None, "/repos/o/r/issues", &query(), 1, 20).has_more,
            Some(false)
        );
        let valid =
            "<https://api.github.com/repos/o/r/issues?state=open&page=2&per_page=20>; rel=\"next\"";
        assert!(
            next_page(
                Some(&format!("{valid}, {valid}")),
                "/repos/o/r/issues",
                &query(),
                1,
                20
            )
            .warning
            .is_some()
        );
        for bad in [
            "",
            "invalid-link-header",
            "<broken> rel=next",
            "<https://api.github.com/x>; rel=\"next",
        ] {
            assert!(
                next_page(Some(bad), "/repos/o/r/issues", &query(), 1, 20)
                    .warning
                    .is_some()
            );
        }
        let prev =
            "<https://api.github.com/repos/o/r/issues?state=open&page=1&per_page=20>; rel=\"prev\"";
        assert_eq!(
            next_page(Some(prev), "/repos/o/r/issues", &query(), 2, 20).has_more,
            Some(false)
        );
    }
}
