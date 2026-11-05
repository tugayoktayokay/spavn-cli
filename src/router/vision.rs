use super::*;


pub fn base64_encode_bytes(data: &[u8]) -> String {
    base64_encode(data)
}

pub(crate) fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct DraggedImage {
    pub raw_span: String,
    pub path: String,
    pub filename: String,
    pub mime: &'static str,
    pub size_bytes: usize,
    pub base64: String,
}

pub(crate) static PASTED_IMAGE_CACHE: std::sync::Mutex<Vec<(String, DraggedImage)>> =
    std::sync::Mutex::new(Vec::new());

pub fn register_read_image(path_key: &str, img: DraggedImage) {
    let marker = format!("[SPAVN Vision Image Loaded: {} (", path_key);
    if let Ok(mut cache) = PASTED_IMAGE_CACHE.lock() {
        cache.retain(|(k, _)| k != &marker);
        cache.push((marker, img));
    }
}

/// Collapses any dragged local image file paths inside `input` into compact OMP-style
/// `[🖼️ Screenshot.png (136KB)]` tokens and caches their `DraggedImage` payload for submission!
pub fn collapse_pasted_image_paths(input: &str) -> (String, bool) {
    let found = extract_dragged_images_raw(input);
    if found.is_empty() {
        return (input.to_string(), false);
    }
    let mut updated = input.to_string();
    let mut changed = false;
    if let Ok(mut cache) = PASTED_IMAGE_CACHE.lock() {
        for img in found {
            let kb = (img.size_bytes as f64 / 1024.0).max(1.0);
            let badge = format!("[🖼️ {} ({:.0}KB)]", img.filename, kb);
            if updated.contains(&img.raw_span) {
                updated = updated.replace(&img.raw_span, &badge);
                changed = true;
            }
            if !cache.iter().any(|(b, _)| b == &badge) {
                cache.push((badge, img));
            }
        }
    }
    (updated, changed)
}

/// Detects local image paths dragged into the terminal (including macOS `\ `-escaped paths)
/// OR previously collapsed `[🖼️ ...]` tokens, returning full metadata + base64.
pub fn extract_dragged_images_info(text: &str) -> Vec<DraggedImage> {
    let mut results = extract_dragged_images_raw(text);
    if let Ok(cache) = PASTED_IMAGE_CACHE.lock() {
        for (badge, img) in cache.iter() {
            if text.contains(badge) && !results.iter().any(|r| r.path == img.path) {
                results.push(img.clone());
            }
        }
    }
    results
}

pub(crate) fn extract_dragged_images_raw(text: &str) -> Vec<DraggedImage> {
    let mut results = Vec::new();
    let exts = [
        (".png", "image/png"),
        (".jpg", "image/jpeg"),
        (".jpeg", "image/jpeg"),
        (".webp", "image/webp"),
        (".gif", "image/gif"),
    ];
    let lower = text.to_lowercase();
    for (ext, mime) in exts {
        let mut search_from = 0usize;
        while let Some(pos) = lower[search_from..].find(ext) {
            let end_idx = search_from + pos + ext.len();
            search_from = end_idx;
            let prefix = &text[..end_idx];
            let mut start_candidates = Vec::new();
            for (i, ch) in prefix.char_indices() {
                if ch == '/' || ch == '~' {
                    if i == 0 {
                        start_candidates.push(i);
                    } else {
                        let prev = prefix[..i].chars().last().unwrap_or(' ');
                        if prev == ' ' || prev == '\'' || prev == '"' || prev == '\n' {
                            start_candidates.push(i);
                        }
                    }
                }
            }
            for &start_idx in start_candidates.iter().rev() {
                let raw_slice = &text[start_idx..end_idx];
                let raw_path = raw_slice
                    .trim_matches(|c| c == '\'' || c == '"')
                    .replace("\\ ", " ");
                let expanded = crate::config::expand_user_path(&raw_path);
                if expanded.is_file() {
                    if let Ok(bytes) = std::fs::read(&expanded) {
                        if bytes.len() <= 15 * 1024 * 1024 {
                            let fname = expanded
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| raw_path.clone());
                            results.push(DraggedImage {
                                raw_span: raw_slice.to_string(),
                                path: expanded.to_string_lossy().to_string(),
                                filename: fname,
                                mime,
                                size_bytes: bytes.len(),
                                base64: base64_encode(&bytes),
                            });
                            break;
                        }
                    }
                }
            }
        }
    }
    results
}

pub(crate) fn extract_dragged_images(text: &str) -> Vec<(&'static str, String)> {
    extract_dragged_images_info(text)
        .into_iter()
        .map(|img| (img.mime, img.base64))
        .collect()
}

pub(crate) fn sanitize_messages_for_openai_jinja(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    for (idx, m) in messages.iter().enumerate() {
        if m.role == "system" && idx > 0 {
            continue;
        }
        if m.role == "user" {
            if let Some(last) = out.last_mut() {
                if last.role == "user" {
                    let prev = last.content.clone().unwrap_or_default();
                    let cur = m.content.clone().unwrap_or_default();
                    if !cur.is_empty() {
                        last.content = Some(if prev.is_empty() {
                            cur
                        } else {
                            format!("{}\n\n{}", prev, cur)
                        });
                    }
                    continue;
                }
            }
        }
        if m.role == "tool" {
            let valid_prev = out
                .last()
                .map(|p| {
                    p.role == "tool"
                        || (p.role == "assistant"
                            && p.tool_calls
                                .as_ref()
                                .map(|t| !t.is_empty())
                                .unwrap_or(false))
                })
                .unwrap_or(false);
            if !valid_prev {
                continue;
            }
        }
        let mut clean = m.clone();
        if let Some(ref mut tcs) = clean.tool_calls {
            for tc in tcs.iter_mut() {
                if serde_json::from_str::<Value>(&tc.function.arguments)
                    .map(|v| !v.is_object())
                    .unwrap_or(true)
                {
                    tc.function.arguments = "{}".to_string();
                }
            }
        }
        out.push(clean);
    }
    out
}

pub(crate) fn serialize_messages_with_vision(messages: &[ChatMessage]) -> Vec<Value> {
    let sanitized = sanitize_messages_for_openai_jinja(messages);
    let last_user_idx = sanitized.iter().rposition(|m| m.role == "user");
    let mut out_vals = Vec::with_capacity(sanitized.len() + 2);
    let total_len = sanitized.len();
    let mut pending_tool_vision_parts: Vec<Value> = Vec::new();

    for (idx, m) in sanitized.iter().enumerate() {
        let mut v = serde_json::to_value(m).unwrap_or_else(|_| json!({}));
        // Strip non-standard fields that foreign upstreams (OpenCode, OpenAI, Groq) reject across protocols
        if let Some(obj) = v.as_object_mut() {
            obj.remove("thought_signature");
            if let Some(tcs) = obj.get_mut("tool_calls").and_then(|t| t.as_array_mut()) {
                for tc in tcs {
                    if let Some(tc_obj) = tc.as_object_mut() {
                        tc_obj.remove("thought_signature");
                    }
                }
            }
        }
        if m.role == "assistant" && v.get("content").is_none() {
            v["content"] = json!("");
        }
        // Keep heavy base64 images attached for the first ~5 tool rounds (10 messages) of the
        // active turn while the model inspects them; beyond that, the visual observations are
        // already in assistant history and re-uploading 1MB+ base64 on every step bloats tokens.
        if Some(idx) == last_user_idx && total_len.saturating_sub(idx) <= 10 {
            if let Some(ref txt) = m.content {
                let imgs = extract_dragged_images(txt);
                if !imgs.is_empty() {
                    let mut parts = vec![json!({ "type": "text", "text": txt })];
                    for (mime, b64) in imgs {
                        parts.push(json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{};base64,{}", mime, b64)
                            }
                        }));
                    }
                    v["content"] = Value::Array(parts);
                }
            }
        }
        out_vals.push(v);

        let in_active_turn = last_user_idx.map(|u| idx >= u).unwrap_or(true);
        if m.role == "tool" && in_active_turn && idx + 6 >= total_len {
            if let Some(txt) = &m.content {
                if txt.starts_with("[SPAVN Vision Image Loaded:") {
                    let imgs = extract_dragged_images(txt);
                    if !imgs.is_empty() {
                        pending_tool_vision_parts.push(json!({
                            "type": "text",
                            "text": format!("[Visual content of tool result `{}`]:", txt)
                        }));
                        for (mime, b64) in imgs {
                            pending_tool_vision_parts.push(json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", mime, b64)
                                }
                            }));
                        }
                    }
                }
            }
        }

        let next_is_tool = sanitized
            .get(idx + 1)
            .map(|n| n.role == "tool")
            .unwrap_or(false);
        if !next_is_tool && !pending_tool_vision_parts.is_empty() {
            let parts = std::mem::take(&mut pending_tool_vision_parts);
            out_vals.push(json!({
                "role": "user",
                "content": parts
            }));
        }
    }
    out_vals
}

