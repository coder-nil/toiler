//! 文件类型判定：MIME / 语言 / 预览类别，与 pi 的 `lib/file-types.ts` 对齐。
//!
//! 三张表说话：
//!
//! * [`image_mime`] / [`audio_mime`] / [`document_mime`]：决定 `?type=read`
//!   是「流原字节」还是「返回 JSON 文本」；
//! * [`language`]：`?type=meta` 与 `?type=read` 的 `language` 字段（前端拿它选
//!   语法高亮与 Markdown 渲染）；
//! * [`document_preview_kind`]：`pdf` / `docx`，决定前端走 iframe 还是 DOCX 转写。

/// 文本预览上限（pi 的 `TEXT_PREVIEW_MAX_BYTES`）。
pub const TEXT_PREVIEW_MAX_BYTES: u64 = 256 * 1024;
/// 图片预览上限（pi 的 `IMAGE_PREVIEW_MAX_BYTES`）。
pub const IMAGE_PREVIEW_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// DOCX 预览上限（pi 的 `DOCX_PREVIEW_MAX_BYTES`）。
pub const DOCX_PREVIEW_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// 文件名（不含目录），与 `path.basename` 同义（同时认 `\` 与 `/`）。
pub fn base_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rsplit(['/', '\\']).next() {
        Some(name) => name,
        None => trimmed,
    }
}

/// 小写扩展名（没有点号时返回空串）。
pub fn extension(path: &str) -> String {
    let base = base_name(path).to_ascii_lowercase();
    match base.rsplit_once('.') {
        Some((_, ext)) => ext.to_string(),
        None => String::new(),
    }
}

fn table_lookup(table: &[(&str, &str)], ext: &str) -> Option<String> {
    table
        .iter()
        .find(|(key, _)| *key == ext)
        .map(|(_, value)| (*value).to_string())
}

const IMAGE_MIME: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    ("bmp", "image/bmp"),
    ("ico", "image/x-icon"),
    ("avif", "image/avif"),
];

const AUDIO_MIME: &[(&str, &str)] = &[
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("oga", "audio/ogg"),
    ("opus", "audio/ogg"),
    ("m4a", "audio/mp4"),
    ("aac", "audio/aac"),
    ("flac", "audio/flac"),
    ("weba", "audio/webm"),
    ("webm", "audio/webm"),
];

const DOCUMENT_MIME: &[(&str, &str)] = &[
    ("pdf", "application/pdf"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
];

/// 扩展名 → 图片 MIME。
pub fn image_mime(path: &str) -> Option<String> {
    table_lookup(IMAGE_MIME, &extension(path))
}

/// 扩展名 → 音频 MIME。
pub fn audio_mime(path: &str) -> Option<String> {
    table_lookup(AUDIO_MIME, &extension(path))
}

/// 扩展名 → 文档 MIME（pdf / docx）。
pub fn document_mime(path: &str) -> Option<String> {
    table_lookup(DOCUMENT_MIME, &extension(path))
}

/// 文档预览类别：`pdf` / `docx`，其它为 `None`。
pub fn document_preview_kind(path: &str) -> Option<&'static str> {
    match extension(path).as_str() {
        "pdf" => Some("pdf"),
        "docx" => Some("docx"),
        _ => None,
    }
}

const EXT_TO_LANGUAGE: &[(&str, &str)] = &[
    ("ts", "typescript"),
    ("tsx", "typescript"),
    ("js", "javascript"),
    ("jsx", "javascript"),
    ("mjs", "javascript"),
    ("cjs", "javascript"),
    ("py", "python"),
    ("rb", "ruby"),
    ("go", "go"),
    ("rs", "rust"),
    ("java", "java"),
    ("kt", "kotlin"),
    ("swift", "swift"),
    ("c", "c"),
    ("cpp", "cpp"),
    ("h", "c"),
    ("hpp", "cpp"),
    ("cs", "csharp"),
    ("html", "html"),
    ("htm", "html"),
    ("css", "css"),
    ("scss", "css"),
    ("less", "css"),
    ("json", "json"),
    ("jsonl", "json"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("toml", "toml"),
    ("xml", "xml"),
    ("md", "markdown"),
    ("mdx", "markdown"),
    ("sh", "bash"),
    ("bash", "bash"),
    ("zsh", "bash"),
    ("fish", "bash"),
    ("sql", "sql"),
    ("graphql", "graphql"),
    ("gql", "graphql"),
    ("dockerfile", "dockerfile"),
    ("tf", "hcl"),
    ("hcl", "hcl"),
    ("env", "bash"),
    ("gitignore", "bash"),
    ("txt", "text"),
    ("pdf", "pdf"),
    ("docx", "word"),
];

/// 语法高亮的语言名（pi 的 `getLanguage`）。
///
/// 整名优先（`Dockerfile` / `.env*` / `Makefile`），其次看扩展名。
pub fn language(path: &str) -> String {
    let base = base_name(path).to_ascii_lowercase();
    if base == "dockerfile" || base.starts_with("dockerfile.") {
        return "dockerfile".to_string();
    }
    if base == ".env" || base.starts_with(".env.") {
        return "bash".to_string();
    }
    if base == "makefile" || base == "gnumakefile" {
        return "makefile".to_string();
    }
    table_lookup(EXT_TO_LANGUAGE, &extension(path)).unwrap_or_else(|| "text".to_string())
}

/// `?type=meta` 返回的 `mime`：图片/音频/文档优先，其余当纯文本。
pub fn meta_mime(path: &str) -> String {
    image_mime(path)
        .or_else(|| audio_mime(path))
        .or_else(|| document_mime(path))
        .unwrap_or_else(|| "text/plain".to_string())
}

/// `?type=download` 用的 MIME：没有可识别类型时给 `application/octet-stream`。
pub fn download_mime(path: &str) -> String {
    image_mime(path)
        .or_else(|| audio_mime(path))
        .or_else(|| document_mime(path))
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

/// `Content-Disposition`：ASCII 兜底名 + RFC 5987 的 UTF-8 名。
///
/// 与 pi 的 `getContentDisposition` 逐字对齐：不可打印字符与 `"\;` 换成 `_`，
/// `filename*` 用百分号编码（`!'()*` 额外转义，避免 RFC 5987 的边界歧义）。
pub fn content_disposition(file_name: &str, as_download: bool) -> String {
    let disposition = if as_download { "attachment" } else { "inline" };
    let mut fallback: String = file_name
        .chars()
        .map(|ch| {
            let code = ch as u32;
            if (0x20..=0x7e).contains(&code) && !matches!(ch, '"' | '\\' | ';' | '\r' | '\n') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if fallback.is_empty() {
        fallback = "download".to_string();
    }
    format!(
        "{disposition}; filename=\"{fallback}\"; filename*=UTF-8''{}",
        encode_header_value(file_name)
    )
}

fn encode_header_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let ch = *byte as char;
        let plain = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~');
        if plain {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_types_map_to_mimes() {
        assert_eq!(image_mime("/a/b.PNG").as_deref(), Some("image/png"));
        assert_eq!(audio_mime("/a/b.mp3").as_deref(), Some("audio/mpeg"));
        assert_eq!(
            document_mime("/a/b.pdf").as_deref(),
            Some("application/pdf")
        );
        assert_eq!(image_mime("/a/b.rs"), None);
    }

    #[test]
    fn language_prefers_whole_names() {
        assert_eq!(language("/a/Dockerfile"), "dockerfile");
        assert_eq!(language("/a/dockerfile.dev"), "dockerfile");
        assert_eq!(language("/a/.env"), "bash");
        assert_eq!(language("/a/.env.local"), "bash");
        assert_eq!(language("/a/Makefile"), "makefile");
        assert_eq!(language("/a/x.rs"), "rust");
        assert_eq!(language("/a/x.unknown"), "text");
        assert_eq!(language("/a/x"), "text");
    }

    #[test]
    fn meta_and_download_mimes_differ_on_fallback() {
        assert_eq!(meta_mime("/a/x.rs"), "text/plain");
        assert_eq!(download_mime("/a/x.rs"), "application/octet-stream");
    }

    #[test]
    fn content_disposition_escapes_quotes_and_unicode() {
        let header = content_disposition("a\"b.txt", false);
        assert!(header.starts_with("inline; filename=\"a_b.txt\""));
        assert!(header.contains("filename*=UTF-8''a%22b.txt"));

        let japanese = content_disposition("プラン.md", true);
        assert!(
            japanese.starts_with("attachment; filename=\"___.md\""),
            "{japanese}"
        );
        assert!(japanese.contains("filename*=UTF-8''%E3%83%97"));
    }

    #[test]
    fn base_name_handles_windows_separators() {
        assert_eq!(base_name(r"C:\proj\a.rs"), "a.rs");
        assert_eq!(base_name("/a/b/"), "b");
    }
}
