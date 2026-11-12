//! DOCX → HTML 预览（`?type=preview`）。
//!
//! pi 用 `mammoth` 做这件事；toiler 没有 Node，所以这里自己解 OOXML：
//! 读 zip 里的 `word/document.xml`，把段落/标题/粗斜体/表格/列表转成 HTML。
//!
//! **明确不做**（与 mammoth 的差距，写在这里以免被当成 bug）：
//!
//! * 图片（`w:drawing`）不内联成 data URI —— 只输出 `[图片]` 占位；
//! * 样式表（`styles.xml`）不解析，只用 `w:pStyle` 的 `Heading1..6` 名字判标题；
//! * 页眉页脚、脚注、批注、公式、域代码、修订痕迹一律跳过。
//!
//! 目标是「打开就能读」，不是像素级还原。

use std::io::Read;

use flate2::read::DeflateDecoder;

/// zip 里一个条目的定位结果。
struct ZipEntry {
    method: u16,
    compressed_size: u64,
    local_header_offset: u64,
}

/// 扫 zip 的中央目录，按名字找条目（不支持 zip64，DOCX 用不到）。
fn find_entry(archive: &[u8], wanted: &str) -> Option<ZipEntry> {
    // EOCD 在文件末尾，最多 22 + 65535 字节注释。
    let scan_start = archive.len().saturating_sub(22 + 0xFFFF);
    let mut eocd = None;
    let mut index = scan_start;
    while index + 4 <= archive.len() {
        if archive[index..index + 4] == [0x50, 0x4b, 0x05, 0x06] {
            eocd = Some(index);
            break;
        }
        index += 1;
    }
    let eocd = eocd?;
    let read_u16 = |at: usize| -> Option<u16> {
        archive
            .get(at..at + 2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
    };
    let read_u32 = |at: usize| -> Option<u32> {
        archive
            .get(at..at + 4)
            .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    };

    let total = read_u16(eocd + 10)? as usize;
    let mut cursor = read_u32(eocd + 16)? as usize;

    for _ in 0..total {
        if archive.get(cursor..cursor + 4)? != [0x50, 0x4b, 0x01, 0x02] {
            return None;
        }
        let method = read_u16(cursor + 10)?;
        let compressed_size = read_u32(cursor + 20)? as u64;
        let name_len = read_u16(cursor + 28)? as usize;
        let extra_len = read_u16(cursor + 30)? as usize;
        let comment_len = read_u16(cursor + 32)? as usize;
        let local_header_offset = read_u32(cursor + 42)? as u64;
        let name = std::str::from_utf8(archive.get(cursor + 46..cursor + 46 + name_len)?).ok()?;
        if name == wanted {
            return Some(ZipEntry {
                method,
                compressed_size,
                local_header_offset,
            });
        }
        cursor += 46 + name_len + extra_len + comment_len;
    }
    None
}

/// 取出条目解压后的字节。
fn read_entry(archive: &[u8], entry: &ZipEntry) -> Option<Vec<u8>> {
    let offset = entry.local_header_offset as usize;
    if archive.get(offset..offset + 4)? != [0x50, 0x4b, 0x03, 0x04] {
        return None;
    }
    let name_len =
        u16::from_le_bytes([*archive.get(offset + 26)?, *archive.get(offset + 27)?]) as usize;
    let extra_len =
        u16::from_le_bytes([*archive.get(offset + 28)?, *archive.get(offset + 29)?]) as usize;
    let data_start = offset + 30 + name_len + extra_len;
    let data_end = data_start.checked_add(entry.compressed_size as usize)?;
    let raw = archive.get(data_start..data_end)?;

    match entry.method {
        // 0 = stored
        0 => Some(raw.to_vec()),
        // 8 = deflate
        8 => {
            let mut out = Vec::new();
            DeflateDecoder::new(raw).read_to_end(&mut out).ok()?;
            Some(out)
        }
        _ => None,
    }
}

/// 一层最小化的 XML 事件：只区分「开标签 / 闭标签 / 文本」。
enum Token {
    Open { name: String, self_closing: bool },
    Close { name: String },
    Text(String),
}

/// 逐字节扫 XML。DOCX 正文只会出现元素、文本与自闭合标签，属性一律跳过。
fn tokenize(xml: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut rest = xml;
    while let Some(pos) = rest.find('<') {
        if pos > 0 {
            let text = &rest[..pos];
            if !text.trim().is_empty() {
                tokens.push(Token::Text(decode_entities(text)));
            }
        }
        let after = &rest[pos..];
        if let Some(end) = after.find('>') {
            let inner = &after[1..end];
            if let Some(name) = inner.strip_prefix('/') {
                tokens.push(Token::Close {
                    name: name.trim().to_string(),
                });
            } else if inner.starts_with('?') || inner.starts_with('!') {
                // 处理指令 / DOCTYPE / 注释：跳过
            } else {
                let self_closing = inner.ends_with('/');
                let trimmed = inner.trim_end_matches('/');
                let name = trimmed.split_whitespace().next().unwrap_or("").to_string();
                tokens.push(Token::Open { name, self_closing });
            }
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
    tokens
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let Some(end) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            other => other
                .strip_prefix("#x")
                .or_else(|| other.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| other.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(ch) => out.push(ch),
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// 标题级别：`w:pStyle w:val="Heading1"` / `Heading 1` / `heading1`。
fn heading_level(style: &str) -> Option<u8> {
    let lower = style.to_ascii_lowercase().replace([' ', '_'], "");
    let digits = lower.strip_prefix("heading")?;
    digits
        .parse::<u8>()
        .ok()
        .filter(|level| (1..=6).contains(level))
}

/// 是否是列表段落（`ListParagraph` 或带 `w:numPr`）。
fn is_list_style(style: &str) -> bool {
    style.eq_ignore_ascii_case("ListParagraph")
}

/// 段落内的一个 run。
struct Run {
    text: String,
    bold: bool,
    italic: bool,
    underline: bool,
}

/// 一个段落 / 表格单元格 / 表格。
enum Block {
    Paragraph {
        runs: Vec<Run>,
        style: String,
        numbered: bool,
        has_drawing: bool,
    },
    Table(Vec<Vec<Vec<Run>>>),
}

/// 把 `word/document.xml` 解析成块序列。
fn parse_blocks(xml: &str) -> Vec<Block> {
    let tokens = tokenize(xml);
    let mut blocks: Vec<Block> = Vec::new();

    // 当前段落的状态
    let mut runs: Vec<Run> = Vec::new();
    let mut style = String::new();
    let mut numbered = false;
    let mut has_drawing = false;
    let mut in_run = false;
    let mut bold = false;
    let mut italic = false;
    let mut underline = false;
    let mut in_text = false;
    let mut skip_depth = 0usize;

    // 表格状态
    let mut table: Option<Vec<Vec<Vec<Run>>>> = None;
    let mut row: Option<Vec<Vec<Run>>> = None;
    let mut cell: Option<Vec<Run>> = None;

    for token in tokens {
        match token {
            Token::Open { name, self_closing } => {
                match name.as_str() {
                    // 这些子树整体跳过（属性里有大量重复的「文本」，读进来只会污染输出）
                    "w:instrText"
                    | "w:delText"
                    | "w:footnoteReference"
                    | "w:commentReference"
                    | "w:annotationRef"
                    | "w:sectPr"
                    | "w:headerReference"
                    | "w:footerReference" => {
                        if !self_closing {
                            skip_depth += 1;
                        }
                    }
                    "w:drawing" | "w:pict" | "w:object" => has_drawing = true,
                    "w:numPr" => numbered = true,
                    "w:p" => {
                        runs.clear();
                        style.clear();
                        numbered = false;
                        has_drawing = false;
                    }
                    "w:r" => {
                        in_run = true;
                        bold = false;
                        italic = false;
                        underline = false;
                    }
                    "w:b" if in_run => bold = true,
                    "w:i" if in_run => italic = true,
                    "w:u" if in_run => underline = true,
                    // `<w:t/>`（空文本）不能把 in_text 打开，否则后面同一段落里的
                    // 兄弟元素文本全被漏掉。
                    "w:t" if !self_closing => in_text = true,
                    "w:tab" => {
                        if in_run {
                            runs.push(Run {
                                text: "\t".to_string(),
                                bold,
                                italic,
                                underline,
                            });
                        }
                    }
                    "w:br" => {
                        if in_run {
                            runs.push(Run {
                                text: "\n".to_string(),
                                bold,
                                italic,
                                underline,
                            });
                        }
                    }
                    "w:tbl" => {
                        table = Some(Vec::new());
                    }
                    "w:tr" if table.is_some() => row = Some(Vec::new()),
                    "w:tc" if row.is_some() => cell = Some(Vec::new()),
                    _ => {}
                }
            }
            Token::Close { name } => {
                match name.as_str() {
                    "w:instrText"
                    | "w:delText"
                    | "w:footnoteReference"
                    | "w:commentReference"
                    | "w:annotationRef"
                    | "w:sectPr"
                    | "w:headerReference"
                    | "w:footerReference" => {
                        skip_depth = skip_depth.saturating_sub(1);
                    }
                    "w:pStyle" => {}
                    "w:r" => in_run = false,
                    "w:t" => in_text = false,
                    "w:p" => {
                        // 单元格里的段落进 cell，正文里的进 blocks。
                        if let Some(current_cell) = cell.as_mut() {
                            current_cell.append(&mut runs);
                            current_cell.push(Run {
                                text: "\n".to_string(),
                                bold: false,
                                italic: false,
                                underline: false,
                            });
                        } else if skip_depth == 0 {
                            let paragraph = Block::Paragraph {
                                runs: std::mem::take(&mut runs),
                                style: style.clone(),
                                numbered,
                                has_drawing,
                            };
                            // 完全空段落也保留（对应文档里的空行），但去掉纯占位
                            blocks.push(paragraph);
                        }
                        runs.clear();
                        style.clear();
                        numbered = false;
                        has_drawing = false;
                    }
                    "w:tc" => {
                        if let (Some(row_ref), Some(mut cell_runs)) = (row.as_mut(), cell.take()) {
                            while matches!(cell_runs.last(), Some(run) if run.text == "\n") {
                                cell_runs.pop();
                            }
                            row_ref.push(cell_runs);
                        }
                    }
                    "w:tr" => {
                        if let (Some(table_ref), Some(row_ref)) = (table.as_mut(), row.take()) {
                            table_ref.push(row_ref);
                        }
                    }
                    "w:tbl" => {
                        if let Some(rows) = table.take() {
                            blocks.push(Block::Table(rows));
                        }
                    }
                    _ => {}
                }
            }
            Token::Text(text) => {
                if skip_depth > 0 {
                    continue;
                }
                if in_text && in_run {
                    runs.push(Run {
                        text,
                        bold,
                        italic,
                        underline,
                    });
                }
            }
        }
    }

    blocks
}

/// 从 `<w:pStyle w:val="Heading1"/>` 里把 `Heading1` 抠出来。
///
/// [`tokenize`] 会丢掉属性，所以这里单独扫一遍原始 XML，把「段落位置 → 样式名」
/// 记录下来。比在分词器里带属性简单，也不会让主循环变复杂。
fn paragraph_styles(xml: &str) -> Vec<String> {
    let mut styles = Vec::new();
    let mut rest = xml;
    while let Some(pos) = rest.find("<w:p>") {
        rest = &rest[pos + 5..];
        let near_end = rest.find("</w:p>").unwrap_or(rest.len());
        let paragraph = &rest[..near_end];
        let style = paragraph
            .find("<w:pStyle")
            .and_then(|start| {
                let tag_end = paragraph[start..].find('>')? + start;
                let tag = &paragraph[start..tag_end];
                let val_start = tag.find("w:val=\"")? + 7;
                let val_end = tag[val_start..].find('"')? + val_start;
                Some(tag[val_start..val_end].to_string())
            })
            .unwrap_or_default();
        styles.push(style);
    }
    styles
}

fn render_runs(runs: &[Run]) -> String {
    let mut out = String::new();
    for run in runs {
        let mut html = escape_html(&run.text);
        if run.text.contains('\n') || run.text.contains('\t') {
            html = html.replace('\n', "<br>").replace('\t', "&emsp;");
        }
        if run.underline {
            html = format!("<u>{html}</u>");
        }
        if run.italic {
            html = format!("<em>{html}</em>");
        }
        if run.bold {
            html = format!("<strong>{html}</strong>");
        }
        out.push_str(&html);
    }
    out
}

fn render_blocks(blocks: &[Block], styles: &[String]) -> String {
    let mut out = String::new();
    let mut paragraph_index = 0usize;
    let mut in_list = false;

    for block in blocks {
        match block {
            Block::Table(rows) => {
                if in_list {
                    out.push_str("</ul>");
                    in_list = false;
                }
                out.push_str("<table>");
                for row in rows {
                    out.push_str("<tr>");
                    for cell in row {
                        out.push_str("<td>");
                        out.push_str(&render_runs(cell));
                        out.push_str("</td>");
                    }
                    out.push_str("</tr>");
                }
                out.push_str("</table>");
            }
            Block::Paragraph {
                runs,
                style,
                numbered,
                has_drawing,
            } => {
                // 段落样式名既可能来自 `w:pStyle` 的属性扫描，也可能来自分词器，
                // 两处取其一（属性扫描更准）。
                let effective_style = styles
                    .get(paragraph_index)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(style)
                    .clone();
                paragraph_index += 1;

                let text_html = render_runs(runs);
                let is_list = *numbered || is_list_style(&effective_style);
                if is_list && !in_list {
                    out.push_str("<ul>");
                    in_list = true;
                } else if !is_list && in_list {
                    out.push_str("</ul>");
                    in_list = false;
                }

                if let Some(level) = heading_level(&effective_style) {
                    out.push_str(&format!("<h{level}>{text_html}</h{level}>"));
                } else if is_list {
                    out.push_str(&format!("<li>{text_html}</li>"));
                } else if text_html.trim().is_empty() && !has_drawing {
                    out.push_str("<p></p>");
                } else if *has_drawing && text_html.trim().is_empty() {
                    out.push_str("<p>[图片]</p>");
                } else {
                    out.push_str(&format!("<p>{text_html}</p>"));
                }
            }
        }
    }
    if in_list {
        out.push_str("</ul>");
    }
    out
}

/// 把 DOCX 字节转成 HTML 片段；解析不出来时返回 `None`。
pub fn docx_to_html(archive: &[u8]) -> Option<String> {
    let entry = find_entry(archive, "word/document.xml")?;
    let xml_bytes = read_entry(archive, &entry)?;
    let xml = String::from_utf8_lossy(&xml_bytes).to_string();
    let body_start = xml.find("<w:body>").unwrap_or(0);
    let body_end = xml.find("</w:body>").unwrap_or(xml.len());
    let body = &xml[body_start..body_end.min(xml.len())];
    let styles = paragraph_styles(body);
    let blocks = parse_blocks(body);
    Some(render_blocks(&blocks, &styles))
}

/// 包一层完整的 HTML 文档（样式与 pi 的 `wrapDocxPreviewHtml` 保持一致）。
pub fn wrap_preview_html(body_html: &str, file_name: &str) -> String {
    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root {{ color-scheme: light; }}
  html, body {{ margin: 0; min-height: 100%; background: #eef1f5; color: #171717; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; padding: 28px; }}
  main {{
    box-sizing: border-box;
    max-width: 840px;
    min-height: calc(100vh - 56px);
    margin: 0 auto;
    padding: 56px 64px;
    background: #fff;
    box-shadow: 0 8px 28px rgba(15, 23, 42, 0.14);
  }}
  .file-title {{
    margin: 0 0 28px;
    padding-bottom: 10px;
    border-bottom: 1px solid #e5e7eb;
    color: #6b7280;
    font: 12px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    word-break: break-word;
  }}
  h1, h2, h3, h4, h5, h6 {{ line-height: 1.3; margin: 1.1em 0 0.45em; color: #111827; }}
  p {{ margin: 0.65em 0; line-height: 1.7; }}
  table {{ border-collapse: collapse; max-width: 100%; margin: 1em 0; }}
  th, td {{ border: 1px solid #d1d5db; padding: 6px 9px; vertical-align: top; }}
  img {{ max-width: 100%; height: auto; }}
  pre {{ white-space: pre-wrap; overflow-wrap: anywhere; }}
  a {{ color: #2563eb; }}
  @media (max-width: 720px) {{
    body {{ padding: 0; background: #fff; }}
    main {{ min-height: 100vh; padding: 28px 22px; box-shadow: none; }}
  }}
</style>
</head>
<body>
<main>
<div class="file-title">{}</div>
{}
</main>
</body>
</html>"#,
        escape_html(file_name),
        body_html
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_style_parsing() {
        assert_eq!(heading_level("Heading1"), Some(1));
        assert_eq!(heading_level("Heading 3"), Some(3));
        assert_eq!(heading_level("heading6"), Some(6));
        assert_eq!(heading_level("Heading7"), None);
        assert_eq!(heading_level("Normal"), None);
    }

    #[test]
    fn entities_decode() {
        assert_eq!(decode_entities("a &amp; b"), "a & b");
        assert_eq!(decode_entities("&#65;"), "A");
        assert_eq!(decode_entities("&#x4E2D;"), "中");
        // 认不出的实体原样保留
        assert_eq!(decode_entities("&unknown;"), "&unknown;");
    }

    #[test]
    fn tokenizer_skips_attributes_and_pi() {
        let tokens = tokenize(r#"<?xml version="1.0"?><w:p><w:r><w:t>hi</w:t></w:r></w:p>"#);
        let has_pi = tokens
            .iter()
            .any(|token| matches!(token, Token::Open { name, .. } if name.starts_with('?')));
        assert!(!has_pi, "处理指令不应变成元素");
        let texts: Vec<&str> = tokens
            .iter()
            .filter_map(|token| match token {
                Token::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["hi"]);
    }

    #[test]
    fn renders_headings_and_bold() {
        let xml = r#"<w:body>
            <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>
            <w:p><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r><w:r><w:t> plain</w:t></w:r></w:p>
        </w:body>"#;
        let styles = paragraph_styles(xml);
        assert_eq!(styles, vec!["Heading1".to_string(), String::new()]);
        let html = render_blocks(&parse_blocks(xml), &styles);
        assert!(html.contains("<h1>Title</h1>"), "{html}");
        assert!(html.contains("<strong>bold</strong>"), "{html}");
        assert!(html.contains(" plain"), "{html}");
    }

    #[test]
    fn renders_tables() {
        let xml = r#"<w:body><w:tbl>
            <w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr>
            <w:tr><w:tc><w:p><w:r><w:t>c</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>d</w:t></w:r></w:p></w:tc></w:tr>
        </w:tbl></w:body>"#;
        let html = render_blocks(&parse_blocks(xml), &paragraph_styles(xml));
        assert!(html.contains("<table>"), "{html}");
        assert!(html.contains("<td>a</td>"), "{html}");
        assert!(html.contains("<td>d</td>"), "{html}");
    }

    #[test]
    fn wraps_document_with_escaped_title() {
        let html = wrap_preview_html("<p>x</p>", "a<b>.docx");
        assert!(html.contains("a&lt;b&gt;.docx"));
        assert!(html.contains("<p>x</p>"));
    }

    #[test]
    fn rejects_non_zip_input() {
        assert!(docx_to_html(b"not a zip").is_none());
    }
}
