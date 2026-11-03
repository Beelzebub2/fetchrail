//! Bounded, offline Metalink 4 (.meta4) inspection. Only the frontend's
//! user-selected XML bytes are accepted, never an arbitrary filesystem path.
//! Import does not fetch any URL or download any file without confirmation.

use quick_xml::{
    events::{BytesStart, Event},
    Reader,
};
use serde::Serialize;
use std::{collections::HashSet, net::IpAddr};
use url::{Host, Url};

const MAX_BYTES: usize = 512 * 1024;
const MAX_MIRRORS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetalinkImport {
    pub file_name: String,
    pub size: Option<u64>,
    pub expected_sha256: String,
    /// Canonical full URLs are returned only to the local add-download form.
    /// They must not be copied into regular download records or diagnostics.
    pub urls: Vec<String>,
}

fn attribute(element: &BytesStart<'_>, key: &str) -> Result<Option<String>, String> {
    for entry in element.attributes().with_checks(true) {
        let entry = entry.map_err(|_| "Invalid Metalink attribute syntax.")?;
        if entry.key.as_ref() == key {
            return Ok(Some(
                entry
                    .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .map_err(|_| "Invalid Metalink attribute encoding.")?
                    .into_owned(),
            ));
        }
    }
    Ok(None)
}

fn local_or_reserved(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 168 || (b == 0 && (c == 0 || c == 2))))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            let segment = ip.segments();
            segment[0] & 0xe000 != 0x2000 || (segment[0] == 0x2001 && segment[1] == 0xdb8)
        }
    }
}

/// Validate URLs before accepting them as equivalent mirrors. This is *not*
/// proof that the URLs have the same contents; only the trusted expected hash
/// permits mixing their bytes, and the final file must match that checksum.
pub fn validate_mirror_url(value: &str, allow_local: bool) -> Result<Url, String> {
    let mut url =
        Url::parse(value.trim()).map_err(|_| "Enter a valid absolute HTTP(S) mirror URL.")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Mirror sources must be HTTP(S) links without embedded credentials.".into());
    }
    if !allow_local {
        let host = url
            .host_str()
            .unwrap_or("")
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if host == "localhost"
            || host.ends_with(".localhost")
            || host.ends_with(".local")
            || host.ends_with(".internal")
            || host.ends_with(".onion")
        {
            return Err("Metalink cannot suggest local or internal mirror URLs.".into());
        }
        match url.host() {
            Some(Host::Ipv4(ip)) if local_or_reserved(IpAddr::V4(ip)) => {
                return Err("Metalink contains a private or reserved IP mirror.".into())
            }
            Some(Host::Ipv6(ip)) if local_or_reserved(IpAddr::V6(ip)) => {
                return Err("Metalink contains a private or reserved IP mirror.".into())
            }
            _ => {}
        }
    }
    url.set_fragment(None);
    Ok(url)
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
        && !name.trim_end_matches([' ', '.']).is_empty()
}

pub fn inspect(xml: &str) -> Result<MetalinkImport, String> {
    if xml.len() > MAX_BYTES {
        return Err("Metalink exceeds the 512 KiB limit.".into());
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut tags: Vec<String> = Vec::new();
    let mut seen_root = false;
    let mut seen_file = false;
    let mut file_name = None;
    let mut size = None;
    let mut sha256 = None;
    let mut urls = Vec::new();
    let mut buffer = String::new();
    let mut selected_hash = false;
    let mut saw_unsupported_hash = false;
    loop {
        match reader
            .read_event()
            .map_err(|_| "Metalink XML is malformed.")?
        {
            Event::DocType(_) => {
                return Err("Metalink XML cannot contain a DTD or custom entities.".into())
            }
            Event::Start(element) => {
                let tag = element.local_name().as_ref().to_string();
                if tags.is_empty() {
                    if seen_root {
                        return Err("Metalink contains more than one root element.".into());
                    }
                    if tag != "metalink" {
                        return Err("Expected a Metalink 4 XML document.".into());
                    }
                    seen_root = true;
                    if attribute(&element, "xmlns")?.as_deref()
                        != Some("urn:ietf:params:xml:ns:metalink")
                    {
                        return Err("Metalink must use the standard Metalink 4 namespace.".into());
                    }
                } else if tags.as_slice() == ["metalink"] && tag == "file" {
                    if seen_file {
                        return Err("Multi-file Metalink imports are not supported; inspect each file separately.".into());
                    }
                    seen_file = true;
                    let name =
                        attribute(&element, "name")?.ok_or("Metalink file requires a name.")?;
                    if !plain_name(&name) {
                        return Err("Metalink contains an unsafe file name.".into());
                    }
                    file_name = Some(name);
                } else if tags.as_slice() == ["metalink", "file"] && tag == "hash" {
                    selected_hash = attribute(&element, "type")?.is_some_and(|hash_type| {
                        hash_type.eq_ignore_ascii_case("sha-256")
                            || hash_type.eq_ignore_ascii_case("sha256")
                    });
                    if !selected_hash {
                        saw_unsupported_hash = true;
                    }
                }
                if tags.len() >= 12 {
                    return Err("Metalink XML nesting is too deep.".into());
                }
                if tags.as_slice() == ["metalink", "file"]
                    && matches!(tag.as_str(), "size" | "hash" | "url")
                {
                    buffer.clear();
                }
                tags.push(tag);
            }
            Event::Empty(element) => {
                if tags.is_empty() && element.local_name().as_ref() == "metalink" {
                    return Err("Metalink cannot be empty.".into());
                }
                if tags.as_slice() == ["metalink"] && element.local_name().as_ref() == "file" {
                    return Err("Metalink file has no metadata or download URLs.".into());
                }
            }
            Event::Text(text) => {
                if tags.as_slice() == ["metalink", "file", "url"]
                    || tags.as_slice() == ["metalink", "file", "hash"]
                    || tags.as_slice() == ["metalink", "file", "size"]
                {
                    let normalized = text.xml10_content();
                    let decoded = quick_xml::escape::unescape(&normalized)
                        .map_err(|_| "Invalid Metalink XML entity.")?;
                    if buffer.len().saturating_add(decoded.len()) > 4096 {
                        return Err("Metalink metadata field exceeds its limit.".into());
                    }
                    buffer.push_str(&decoded);
                }
            }
            Event::GeneralRef(reference) => {
                // quick-xml 0.42 emits entity references separately from Text.
                // Ignoring `&amp;` would silently rewrite signed URLs, while
                // accepting custom entities would enable hostile XML expansion.
                let replacement = match reference.as_ref() {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    _ => reference
                        .resolve_char_ref()
                        .map_err(|_| "Metalink has an invalid character reference.")?
                        .ok_or("Metalink has a forbidden custom XML entity.")?,
                };
                if tags.as_slice() == ["metalink", "file", "url"]
                    || tags.as_slice() == ["metalink", "file", "hash"]
                    || tags.as_slice() == ["metalink", "file", "size"]
                {
                    if buffer.len().saturating_add(replacement.len_utf8()) > 4096 {
                        return Err("Metalink metadata field exceeds its limit.".into());
                    }
                    buffer.push(replacement);
                }
            }
            Event::CData(_) => return Err("Metalink CDATA is not supported.".into()),
            Event::End(end) => {
                let name = end.local_name();
                let tag = name.as_ref();
                if tags.last().is_none_or(|active| active != tag) {
                    return Err("Metalink XML has mismatched elements.".into());
                }
                if tags.as_slice() == ["metalink", "file", "hash"] && selected_hash {
                    let hash = buffer.trim().to_ascii_lowercase();
                    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Err(
                            "Metalink SHA-256 must be exactly 64 hexadecimal characters.".into(),
                        );
                    }
                    if sha256.replace(hash).is_some() {
                        return Err("Metalink provides conflicting SHA-256 hashes.".into());
                    }
                } else if tags.as_slice() == ["metalink", "file", "size"] {
                    let value = buffer
                        .trim()
                        .parse::<u64>()
                        .map_err(|_| "Metalink contains an invalid file size.")?;
                    if value == 0 || value > 9_007_199_254_740_991 || size.replace(value).is_some()
                    {
                        return Err(
                            "Metalink file size must be a single positive safe integer.".into()
                        );
                    }
                } else if tags.as_slice() == ["metalink", "file", "url"] {
                    if urls.len() >= MAX_MIRRORS {
                        return Err("Metalink supports at most eight HTTP(S) mirrors.".into());
                    }
                    urls.push(validate_mirror_url(buffer.trim(), false)?.to_string());
                }
                tags.pop();
                buffer.clear();
                if tag == "hash" {
                    selected_hash = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !seen_root || !seen_file || !tags.is_empty() {
        return Err("Metalink document is incomplete.".into());
    }
    let sha256 = sha256.ok_or(if saw_unsupported_hash {
        "Only SHA-256 Metalink hashes are supported."
    } else {
        "Metalink requires a trusted SHA-256 checksum."
    })?;
    let mut seen_urls = HashSet::new();
    urls.retain(|url| seen_urls.insert(url.clone()));
    if urls.is_empty() {
        return Err("Metalink must contain at least one HTTP(S) mirror URL.".into());
    }
    Ok(MetalinkImport {
        file_name: file_name.unwrap(),
        size,
        expected_sha256: sha256,
        urls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        format!(
            r#"<?xml version="1.0"?><metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="archive.zip"><size>1024</size><hash type="sha-256">{}</hash><url priority="1">https://a.example.org/archive.zip?key=secret&amp;v=1</url><url priority="2">https://b.example.org/archive.zip</url></file></metalink>"#,
            "a".repeat(64)
        )
    }

    #[test]
    fn imports_bounded_single_file_with_checksum_and_mirrors() {
        let item = inspect(&fixture()).unwrap();
        assert_eq!(item.size, Some(1024));
        assert_eq!(item.file_name, "archive.zip");
        assert_eq!(item.urls.len(), 2);
        assert_eq!(
            item.urls[0],
            "https://a.example.org/archive.zip?key=secret&v=1"
        );
        assert_eq!(item.expected_sha256, "a".repeat(64));
    }

    #[test]
    fn rejects_dtd_entities_path_traversal_private_urls_and_missing_sha() {
        let xml = fixture();
        assert!(inspect(&format!(
            "<!DOCTYPE metalink [<!ENTITY x SYSTEM 'file:///secret'>]>{xml}"
        ))
        .is_err());
        for needle in ["../archive.zip", "C:\\archive.zip", "dir/archive.zip"] {
            assert!(
                inspect(&xml.replace("name=\"archive.zip\"", &format!("name=\"{needle}\"")))
                    .is_err()
            );
        }
        for url in [
            "http://127.0.0.1/a",
            "http://169.254.169.254/a",
            "http://[::1]/a",
            "http://localhost/a",
            "file:///etc/passwd",
            "https://user:pass@example.com/a",
        ] {
            assert!(
                inspect(&xml.replace("https://b.example.org/archive.zip", url)).is_err(),
                "{url}"
            );
        }
        assert!(inspect(&xml.replace("sha-256", "md5")).is_err());
        assert!(
            inspect(&xml.replace("</file>", "</file><file name=\"second.zip\"></file>")).is_err()
        );
        assert!(inspect(&"X".repeat(MAX_BYTES + 1)).is_err());
        assert!(inspect(&xml.replace(&"a".repeat(64), "bad")).is_err());
    }
}
