use super::*;

const MAX_MANIFEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const MAX_VARIANTS: usize = 64;
const MAX_AUDIO_TRACKS: usize = 32;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StreamVariantOption {
    pub id: String,
    pub label: String,
    pub resolution: Option<String>,
    pub bandwidth: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StreamAudioOption {
    pub id: String,
    pub label: String,
    pub language: Option<String>,
    pub group_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StreamManifestInfo {
    pub kind: String,
    pub variants: Vec<StreamVariantOption>,
    pub audio_tracks: Vec<StreamAudioOption>,
    pub default_variant_id: Option<String>,
    pub default_audio_id: Option<String>,
    pub drm: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct StreamDownloadPlan {
    pub kind: String,
    pub input_url: String,
    pub secondary_audio_url: Option<String>,
    pub filtered_mpd: Option<String>,
    pub preferred_container: String,
    pub final_url: Url,
}

pub(crate) async fn inspect_manifest(
    client: &Client,
    url: &Url,
    context: Option<&BrowserRequestContext>,
    health: Option<&crate::health::Health>,
) -> EngineResult<Option<StreamManifestInfo>> {
    if !matches!(url.scheme(), "http" | "https") {
        return Ok(None);
    }
    let fetched = fetch_manifest(client, url, context, health).await?;
    detect_manifest(&fetched.text, fetched.content_type.as_deref(), &fetched.final_url)
}

pub(crate) fn is_manifest_url(url: &Url) -> bool {
    url.path()
        .rsplit('.')
        .next()
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "m3u8" | "mpd"))
}

pub(crate) async fn build_download_plan(
    client: &Client,
    url: &Url,
    context: Option<&BrowserRequestContext>,
    health: Option<&crate::health::Health>,
    selection: Option<&crate::model::StreamSelection>,
) -> EngineResult<Option<(StreamManifestInfo, StreamDownloadPlan)>> {
    let fetched = match fetch_manifest(client, url, context, health).await {
        Ok(value) => value,
        Err(error) => {
            if matches!(&error, EngineError::Http(_)) {
                return Ok(None);
            }
            return Err(error);
        }
    };
    let Some(info) = detect_manifest(&fetched.text, fetched.content_type.as_deref(), &fetched.final_url)? else {
        return Ok(None);
    };
    if info.drm {
        return Err(EngineError::Message(
            "This stream is DRM-protected and cannot be downloaded by Fetchrail.".into(),
        ));
    }
    let selection_kind = selection
        .map(|value| value.kind.to_ascii_lowercase())
        .unwrap_or_default();
    if !selection_kind.is_empty() && selection_kind != info.kind {
        return Err(EngineError::Message(
            "The selected stream profile does not match this manifest.".into(),
        ));
    }
    let preferred_container = selection
        .and_then(|value| value.container.clone())
        .unwrap_or_else(|| "mp4".into());

    if info.kind == "hls" {
        let plan = hls_plan(&fetched, &info, selection, preferred_container)?;
        return Ok(Some((info, plan)));
    }
    let plan = dash_plan(&fetched, &info, selection, preferred_container)?;
    Ok(Some((info, plan)))
}

#[derive(Debug, Clone)]
struct FetchedManifest {
    final_url: Url,
    text: String,
    content_type: Option<String>,
}

async fn fetch_manifest(
    client: &Client,
    url: &Url,
    context: Option<&BrowserRequestContext>,
    health: Option<&crate::health::Health>,
) -> EngineResult<FetchedManifest> {
    let cancel = CancellationToken::new();
    let mut current = url.clone();
    let mut seen = HashSet::new();
    for _ in 0..=MAX_REDIRECTS {
        if !seen.insert(current.clone()) {
            return Err(EngineError::Message(
                "Manifest redirects are looping.".into(),
            ));
        }
        let request = client
            .get(current.clone())
            .headers(context_headers(context)?)
            .header(ACCEPT_ENCODING, "identity");
        let (response, _origin, _global) = send_admitted(
            client,
            request,
            &cancel,
            health,
            #[cfg(feature = "test-tools")]
            None,
        )
        .await?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    EngineError::Message("Manifest redirect is missing a location.".into())
                })?;
            let next = current.join(location).map_err(|_| {
                EngineError::Message("Manifest redirect target is invalid.".into())
            })?;
            if !matches!(next.scheme(), "http" | "https") {
                return Err(EngineError::Message(
                    "Manifest redirects must stay on HTTP or HTTPS.".into(),
                ));
            }
            if current.scheme() == "https" && next.scheme() != "https" {
                return Err(EngineError::Message(
                    "Manifest redirects cannot downgrade HTTPS.".into(),
                ));
            }
            if context.is_some() && current.origin() != next.origin() {
                return Err(EngineError::Message(
                    "Browser session headers are scoped to one site. Add this stream directly or use a same-origin URL."
                        .into(),
                ));
            }
            current = next;
            continue;
        }
        if !status.is_success() {
            return Err(EngineError::Http(status));
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());
        let content_length = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok());
        if content_length.is_some_and(|value| value > MAX_MANIFEST_BYTES) {
            return Err(EngineError::Message(
                "Manifest is too large for safe inspection.".into(),
            ));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            body.extend_from_slice(&chunk);
            if body.len() > MAX_MANIFEST_BYTES {
                return Err(EngineError::Message(
                    "Manifest is too large for safe inspection.".into(),
                ));
            }
        }
        let text = String::from_utf8_lossy(&body).into_owned();
        return Ok(FetchedManifest {
            final_url: current,
            text,
            content_type,
        });
    }
    Err(EngineError::Message(
        "Manifest redirected too many times.".into(),
    ))
}

fn detect_manifest(
    text: &str,
    content_type: Option<&str>,
    final_url: &Url,
) -> EngineResult<Option<StreamManifestInfo>> {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with("#EXTM3U") {
        return Ok(Some(parse_hls_manifest(trimmed, final_url)?));
    }
    if trimmed.contains("<MPD") {
        return Ok(Some(parse_dash_manifest(trimmed)?));
    }
    let looks_like_manifest = final_url
        .path()
        .rsplit('.')
        .next()
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "m3u8" | "mpd"));
    let hint_manifest = content_type.is_some_and(|value| {
        let mime = value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        mime.contains("mpegurl") || mime.contains("dash+xml")
    });
    if looks_like_manifest || hint_manifest {
        return Err(EngineError::Message(
            "Manifest inspection failed: expected a plain-text HLS or DASH manifest.".into(),
        ));
    }
    Ok(None)
}

fn hls_plan(
    fetched: &FetchedManifest,
    info: &StreamManifestInfo,
    selection: Option<&crate::model::StreamSelection>,
    preferred_container: String,
) -> EngineResult<StreamDownloadPlan> {
    let parsed = parse_hls_manifest(&fetched.text, &fetched.final_url)?;
    let variant_id = selection
        .and_then(|value| value.variant_id.as_deref())
        .or(parsed.default_variant_id.as_deref())
        .ok_or_else(|| EngineError::Message("No playable HLS variant was found.".into()))?;
    let variant = parsed
        .variants
        .iter()
        .find(|value| value.id == variant_id)
        .ok_or_else(|| EngineError::Message("The selected HLS quality is no longer available.".into()))?;
    let variant_url = variant
        .label
        .strip_prefix("url:")
        .ok_or_else(|| EngineError::Message("The selected HLS variant is invalid.".into()))?;
    let mut secondary_audio_url = None;
    if !parsed.audio_tracks.is_empty() {
        let audio_id = selection
            .and_then(|value| value.audio_id.as_deref())
            .or(parsed.default_audio_id.as_deref());
        if let Some(audio_id) = audio_id {
            if let Some(audio) = parsed.audio_tracks.iter().find(|value| value.id == audio_id) {
                if let Some(url) = audio.label.strip_prefix("url:") {
                    secondary_audio_url = Some(url.to_string());
                }
            }
        }
    }
    let _ = info;
    Ok(StreamDownloadPlan {
        kind: "hls".into(),
        input_url: variant_url.to_string(),
        secondary_audio_url,
        filtered_mpd: None,
        preferred_container,
        final_url: fetched.final_url.clone(),
    })
}

fn dash_plan(
    fetched: &FetchedManifest,
    info: &StreamManifestInfo,
    selection: Option<&crate::model::StreamSelection>,
    preferred_container: String,
) -> EngineResult<StreamDownloadPlan> {
    let video_id = selection
        .and_then(|value| value.variant_id.as_deref())
        .or(info.default_variant_id.as_deref());
    let audio_id = selection
        .and_then(|value| value.audio_id.as_deref())
        .or(info.default_audio_id.as_deref());
    let filtered = filter_dash_mpd(&fetched.text, video_id, audio_id)?;
    Ok(StreamDownloadPlan {
        kind: "dash".into(),
        input_url: fetched.final_url.to_string(),
        secondary_audio_url: None,
        filtered_mpd: Some(filtered),
        preferred_container,
        final_url: fetched.final_url.clone(),
    })
}

fn parse_hls_manifest(text: &str, base: &Url) -> EngineResult<StreamManifestInfo> {
    let mut variants = Vec::new();
    let mut audios = Vec::new();
    let mut pending_stream: Option<HashMap<String, String>> = None;
    let mut drm = false;
    let mut media_lines = 0usize;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(attributes) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending_stream = Some(parse_attributes(attributes));
            continue;
        }
        if let Some(attributes) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attrs = parse_attributes(attributes);
            if attrs
                .get("TYPE")
                .is_some_and(|value| value.eq_ignore_ascii_case("AUDIO"))
            {
                if let Some(uri) = attrs.get("URI") {
                    let resolved = base
                        .join(uri)
                        .map_err(|_| EngineError::Message("HLS audio URL is invalid.".into()))?;
                    validate_child_url(&resolved, base)?;
                    audios.push(StreamAudioOption {
                        id: format!("a{}", audios.len()),
                        label: format!("url:{resolved}"),
                        language: attrs.get("LANGUAGE").cloned(),
                        group_id: attrs.get("GROUP-ID").cloned(),
                    });
                }
            }
            continue;
        }
        if let Some(attributes) = line.strip_prefix("#EXT-X-KEY:") {
            let attrs = parse_attributes(attributes);
            if attrs
                .get("METHOD")
                .is_some_and(|value| !value.eq_ignore_ascii_case("NONE"))
            {
                drm = true;
            }
            continue;
        }
        if line.starts_with("#EXT-X-SESSION-KEY") {
            drm = true;
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if let Some(attrs) = pending_stream.take() {
            let resolved = base
                .join(line)
                .map_err(|_| EngineError::Message("HLS variant URL is invalid.".into()))?;
            validate_child_url(&resolved, base)?;
            let resolution = attrs.get("RESOLUTION").cloned();
            let bandwidth = attrs.get("BANDWIDTH").and_then(|value| value.parse().ok());
            variants.push(StreamVariantOption {
                id: format!("v{}", variants.len()),
                label: format!("url:{resolved}"),
                resolution,
                bandwidth,
            });
            if variants.len() > MAX_VARIANTS {
                return Err(EngineError::Message(
                    "Manifest exposes too many HLS variants.".into(),
                ));
            }
        } else {
            media_lines += 1;
            if media_lines > 20_000 {
                return Err(EngineError::Message(
                    "Manifest exposes too many HLS segments.".into(),
                ));
            }
        }
    }
    if variants.is_empty() {
        variants.push(StreamVariantOption {
            id: "v0".into(),
            label: format!("url:{base}"),
            resolution: None,
            bandwidth: None,
        });
    }
    if audios.len() > MAX_AUDIO_TRACKS {
        return Err(EngineError::Message(
            "Manifest exposes too many HLS audio tracks.".into(),
        ));
    }
    let default_variant = variants
        .iter()
        .max_by_key(|value| {
            let height = value
                .resolution
                .as_deref()
                .and_then(|resolution| resolution.split('x').nth(1))
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            (height, value.bandwidth.unwrap_or(0))
        })
        .map(|value| value.id.clone());
    let default_audio = audios.first().map(|value| value.id.clone());
    Ok(StreamManifestInfo {
        kind: "hls".into(),
        variants,
        audio_tracks: audios,
        default_variant_id: default_variant,
        default_audio_id: default_audio,
        drm,
        warning: Some("Stream resume is not yet supported; interrupted downloads restart from the beginning.".into()),
    })
}

fn parse_dash_manifest(text: &str) -> EngineResult<StreamManifestInfo> {
    let drm = text.contains("ContentProtection")
        || text.contains("urn:mpeg:dash:mp4protection")
        || text.to_ascii_lowercase().contains("widevine");
    let mut variants = Vec::new();
    let mut audios = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = text[cursor..].find("<AdaptationSet") {
        let start = cursor + relative_start;
        let open_end = text[start..]
            .find('>')
            .map(|index| start + index)
            .ok_or_else(|| EngineError::Message("Malformed DASH adaptation set.".into()))?;
        let attrs = parse_attributes(&text[start + "<AdaptationSet".len()..open_end]);
        let close = text[open_end..]
            .find("</AdaptationSet>")
            .map(|index| open_end + index)
            .ok_or_else(|| EngineError::Message("Malformed DASH adaptation set.".into()))?;
        let block = &text[open_end + 1..close];
        let content_type = attrs
            .get("contentType")
            .cloned()
            .or_else(|| attrs.get("mimeType").cloned())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut rep_cursor = 0;
        while let Some(relative) = block[rep_cursor..].find("<Representation") {
            let rep_start = rep_cursor + relative;
            let rep_open_end = block[rep_start..]
                .find('>')
                .map(|index| rep_start + index)
                .ok_or_else(|| EngineError::Message("Malformed DASH representation.".into()))?;
            let rep_attrs = parse_attributes(
                &block[rep_start + "<Representation".len()..rep_open_end],
            );
            let id = rep_attrs
                .get("id")
                .cloned()
                .unwrap_or_else(|| format!("r{}", variants.len() + audios.len()));
            let bandwidth = rep_attrs
                .get("bandwidth")
                .and_then(|value| value.parse::<u64>().ok());
            let width = rep_attrs
                .get("width")
                .and_then(|value| value.parse::<u64>().ok());
            let height = rep_attrs
                .get("height")
                .and_then(|value| value.parse::<u64>().ok());
            let mime = rep_attrs
                .get("mimeType")
                .cloned()
                .unwrap_or_else(|| content_type.clone());
            if mime.contains("audio") || content_type.contains("audio") {
                audios.push(StreamAudioOption {
                    id,
                    label: rep_attrs
                        .get("codecs")
                        .cloned()
                        .unwrap_or_else(|| "audio".into()),
                    language: attrs.get("lang").cloned(),
                    group_id: None,
                });
                if audios.len() > MAX_AUDIO_TRACKS {
                    return Err(EngineError::Message(
                        "Manifest exposes too many DASH audio tracks.".into(),
                    ));
                }
            } else if mime.contains("video") || content_type.contains("video") || (!mime.contains("audio") && !content_type.contains("audio")) {
                variants.push(StreamVariantOption {
                    id,
                    label: if let (Some(width), Some(height)) = (width, height) {
                        format!("{width}x{height}")
                    } else {
                        "video".into()
                    },
                    resolution: match (width, height) {
                        (Some(width), Some(height)) => Some(format!("{width}x{height}")),
                        _ => None,
                    },
                    bandwidth,
                });
                if variants.len() > MAX_VARIANTS {
                    return Err(EngineError::Message(
                        "Manifest exposes too many DASH variants.".into(),
                    ));
                }
            }
            rep_cursor = rep_open_end + 1;
        }
        cursor = close + "</AdaptationSet>".len();
    }
    if variants.is_empty() {
        return Err(EngineError::Message(
            "No playable DASH video representation was found.".into(),
        ));
    }
    let default_variant = variants
        .iter()
        .max_by_key(|value| {
            let height = value
                .resolution
                .as_deref()
                .and_then(|resolution| resolution.split('x').nth(1))
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            (height, value.bandwidth.unwrap_or(0))
        })
        .map(|value| value.id.clone());
    let default_audio = audios.first().map(|value| value.id.clone());
    Ok(StreamManifestInfo {
        kind: "dash".into(),
        variants,
        audio_tracks: audios,
        default_variant_id: default_variant,
        default_audio_id: default_audio,
        drm,
        warning: Some("Stream resume is not yet supported; interrupted downloads restart from the beginning.".into()),
    })
}

fn filter_dash_mpd(
    text: &str,
    video_id: Option<&str>,
    audio_id: Option<&str>,
) -> EngineResult<String> {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative_start) = text[cursor..].find("<AdaptationSet") {
        let start = cursor + relative_start;
        output.push_str(&text[cursor..start]);
        let open_end = text[start..]
            .find('>')
            .map(|index| start + index)
            .ok_or_else(|| EngineError::Message("Malformed DASH adaptation set.".into()))?;
        let attrs = parse_attributes(&text[start + "<AdaptationSet".len()..open_end]);
        let close = text[open_end..]
            .find("</AdaptationSet>")
            .map(|index| open_end + index)
            .ok_or_else(|| EngineError::Message("Malformed DASH adaptation set.".into()))?;
        let close_end = close + "</AdaptationSet>".len();
        let block = &text[start..close_end];
        let kind = attrs
            .get("contentType")
            .cloned()
            .or_else(|| attrs.get("mimeType").cloned())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let target = if kind.contains("audio") {
            audio_id
        } else if kind.contains("video") {
            video_id
        } else {
            None
        };
        if let Some(target_id) = target {
            output.push_str(&filter_dash_adaptation_block(block, target_id));
        } else {
            output.push_str(block);
        }
        cursor = close_end;
    }
    output.push_str(&text[cursor..]);
    Ok(output)
}

fn filter_dash_adaptation_block(block: &str, target_id: &str) -> String {
    let mut output = String::with_capacity(block.len());
    let open_end = match block.find('>') {
        Some(index) => index,
        None => return block.to_string(),
    };
    let close_start = match block.rfind("</AdaptationSet>") {
        Some(index) => index,
        None => return block.to_string(),
    };
    output.push_str(&block[..=open_end]);
    let body = &block[open_end + 1..close_start];
    let mut cursor = 0;
    let mut kept = false;
    while let Some(relative) = body[cursor..].find("<Representation") {
        let start = cursor + relative;
        output.push_str(&body[cursor..start]);
        let tag_end = match body[start..].find('>') {
            Some(index) => start + index,
            None => break,
        };
        let attrs = parse_attributes(&body[start + "<Representation".len()..tag_end]);
        let id = attrs.get("id");
        let self_closing = body[tag_end.saturating_sub(1)..=tag_end].starts_with("/");
        let end = if self_closing {
            tag_end + 1
        } else {
            body[tag_end + 1..]
                .find("</Representation>")
                .map(|index| tag_end + 1 + index + "</Representation>".len())
                .unwrap_or(tag_end + 1)
        };
        if id.is_some_and(|value| value == target_id) {
            output.push_str(&body[start..end]);
            kept = true;
        }
        cursor = end;
    }
    output.push_str(&body[cursor..]);
    if !kept {
        return block.to_string();
    }
    output.push_str("</AdaptationSet>");
    output
}

fn validate_child_url(child: &Url, parent: &Url) -> EngineResult<()> {
    if !matches!(child.scheme(), "http" | "https") {
        return Err(EngineError::Message(
            "Manifest referenced an unsupported child URL scheme.".into(),
        ));
    }
    if parent.scheme() == "https" && child.scheme() != "https" {
        return Err(EngineError::Message(
            "Manifest child URLs cannot downgrade HTTPS.".into(),
        ));
    }
    Ok(())
}

fn parse_attributes(input: &str) -> HashMap<String, String> {
    let mut attributes = HashMap::new();
    let mut cursor = 0;
    let bytes = input.as_bytes();
    while cursor < bytes.len() {
        while cursor < bytes.len() && (bytes[cursor] == b',' || bytes[cursor].is_ascii_whitespace()) {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            break;
        }
        let start = cursor;
        while cursor < bytes.len() && bytes[cursor] != b'=' {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            break;
        }
        let key = input[start..cursor].trim().trim_matches('/').to_string();
        cursor += 1;
        if cursor >= bytes.len() {
            break;
        }
        let value = if bytes[cursor] == b'\"' {
            cursor += 1;
            let value_start = cursor;
            while cursor < bytes.len() && bytes[cursor] != b'\"' {
                cursor += 1;
            }
            let value = input[value_start..cursor].to_string();
            if cursor < bytes.len() {
                cursor += 1;
            }
            value
        } else {
            let value_start = cursor;
            while cursor < bytes.len() && bytes[cursor] != b',' && bytes[cursor] != b'>' {
                cursor += 1;
            }
            input[value_start..cursor].trim().trim_end_matches('/').to_string()
        };
        if !key.is_empty() {
            attributes.insert(key, value);
        }
    }
    attributes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hls_master_defaults_to_best() {
        let base = Url::parse("https://example.com/master.m3u8").unwrap();
        let manifest = r#"#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",LANGUAGE="en",NAME="English",URI="audio/en.m3u8"
#EXT-X-STREAM-INF:BANDWIDTH=1200000,RESOLUTION=854x480,AUDIO="aud"
video/480.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=3200000,RESOLUTION=1920x1080,AUDIO="aud"
video/1080.m3u8
"#;
        let parsed = parse_hls_manifest(manifest, &base).unwrap();
        assert_eq!(parsed.kind, "hls");
        assert_eq!(parsed.variants.len(), 2);
        assert_eq!(parsed.audio_tracks.len(), 1);
        assert_eq!(parsed.default_variant_id.as_deref(), Some("v1"));
    }

    #[test]
    fn rejects_drm_hls() {
        let base = Url::parse("https://example.com/master.m3u8").unwrap();
        let manifest = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://abc\"\n";
        let parsed = parse_hls_manifest(manifest, &base).unwrap();
        assert!(parsed.drm);
    }

    #[test]
    fn parses_dash_representations() {
        let mpd = r#"<MPD><Period>
<AdaptationSet contentType="video">
<Representation id="360p" bandwidth="800000" width="640" height="360" codecs="avc1" />
<Representation id="1080p" bandwidth="4000000" width="1920" height="1080" codecs="avc1" />
</AdaptationSet>
<AdaptationSet contentType="audio" lang="en">
<Representation id="aud-en" bandwidth="128000" codecs="mp4a" />
</AdaptationSet>
</Period></MPD>"#;
        let parsed = parse_dash_manifest(mpd).unwrap();
        assert_eq!(parsed.kind, "dash");
        assert_eq!(parsed.variants.len(), 2);
        assert_eq!(parsed.default_variant_id.as_deref(), Some("1080p"));
        assert_eq!(parsed.audio_tracks.len(), 1);
    }

    #[test]
    fn detects_dash_drm() {
        let mpd = "<MPD><Period><AdaptationSet><ContentProtection schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\"/></AdaptationSet></Period></MPD>";
        let parsed = parse_dash_manifest(mpd).unwrap();
        assert!(parsed.drm);
    }
}
