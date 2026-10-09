//! Native reflow conversion and a private, bounded adapter for bundled libmobi.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use image::{ImageFormat, ImageReader, Limits};
use roxmltree::{Document, Node, ParsingOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    AppError, BookFormat, BookMetadata, ConversionCapabilities, EpubDocument, Result,
    epub::{escape_xml, resolve_href},
};

const MAX_TEXT_BYTES: u64 = 12 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ASSET_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_OUTPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XML_NODES: u32 = 500_000;
const MAX_MARKUP_DEPTH: usize = 256;
const MAX_CHILD_LOG_BYTES: usize = 64 * 1024;
const MOBI_TIMEOUT: Duration = Duration::from_secs(60);
const PALMDOC_RECORD_BYTES: usize = 4096;
const MOBI_HEADER_BYTES: usize = 232;
const XHTML: &str = "http://www.w3.org/1999/xhtml";
const FB2: &str = "http://www.gribuser.ru/xml/fictionbook/2.0";
const XLINK: &str = "http://www.w3.org/1999/xlink";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionReport {
    pub source_format: BookFormat,
    pub target_format: BookFormat,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub section_count: u64,
    pub images_preserved: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Converter {
    mobitool_path: PathBuf,
}

impl Converter {
    pub fn new(mobitool_path: PathBuf) -> Self {
        Self { mobitool_path }
    }

    pub fn capabilities(&self) -> Vec<ConversionCapabilities> {
        let outputs = vec![
            BookFormat::Epub,
            BookFormat::Txt,
            BookFormat::Html,
            BookFormat::Fb2,
            BookFormat::Mobi,
        ];
        let mut result = vec![ConversionCapabilities {
            inputs: vec![BookFormat::Epub, BookFormat::Txt, BookFormat::Html, BookFormat::Fb2],
            outputs: outputs.clone(),
            warnings: vec!["Native reflow conversion preserves text, paragraphs and JPEG/PNG images; source layout, CSS, fonts and interactive content may be flattened".into()],
        }];
        result.push(ConversionCapabilities {
            inputs: vec![BookFormat::Mobi, BookFormat::Azw3],
            outputs: if self.engine_available() { outputs } else { Vec::new() },
            warnings: vec![if self.engine_available() { "MOBI/KF8/AZW3 input is reconstructed by the bundled libmobi engine; DRM is unsupported".into() } else { "The bundled MOBI input engine is unavailable".into() }],
        });
        result.push(ConversionCapabilities {
            inputs: vec![BookFormat::Pdf, BookFormat::Cbz],
            outputs: Vec::new(),
            warnings: vec![
                "PDF and CBZ are stored in their original format; reflow conversion is unavailable"
                    .into(),
            ],
        });
        result
    }

    pub fn convert(
        &self,
        source: &Path,
        destination: &Path,
        target: BookFormat,
        metadata: &BookMetadata,
    ) -> Result<ConversionReport> {
        validate_destination(destination)?;
        let source_metadata = fs::symlink_metadata(source)?;
        if !source_metadata.file_type().is_file() || source_metadata.len() > MAX_SOURCE_BYTES {
            return Err(invalid("Conversion source must be a regular, bounded file"));
        }
        let source_format = source_format(source)?;
        if matches!(source_format, BookFormat::Pdf | BookFormat::Cbz)
            || matches!(target, BookFormat::Pdf | BookFormat::Cbz | BookFormat::Azw3)
        {
            return Err(AppError::Unsupported(
                "This format is stored without reflow conversion".into(),
            ));
        }
        let staging = StagingDirectory::new()?;
        let mut warnings = Vec::new();
        let mut epub = match source_format {
            BookFormat::Epub => Some(EpubDocument::open(source)?),
            BookFormat::Mobi | BookFormat::Azw3 => {
                warnings.push("MOBI reconstruction may adjust source layout and navigation".into());
                Some(self.reconstruct_mobi(source, &staging, &mut warnings)?)
            }
            _ => None,
        };
        let mut metadata = metadata.clone();
        if metadata.title.trim().is_empty() {
            metadata.title = source
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("Untitled")
                .to_owned();
        }
        if metadata.language.trim().is_empty() {
            metadata.language = "und".into();
        }
        validate_metadata(&metadata)?;
        if target == BookFormat::Epub
            && let Some(document) = epub.as_mut()
        {
            document.update_metadata(&metadata)?;
            document.write(destination, 9)?;
            return Ok(ConversionReport {
                source_format,
                target_format: target,
                before_bytes: source_metadata.len(),
                after_bytes: fs::metadata(destination)?.len(),
                section_count: document.spine.len() as u64,
                images_preserved: document
                    .manifest
                    .iter()
                    .filter(|item| item.media_type.starts_with("image/"))
                    .count() as u64,
                warnings,
            });
        }
        let mut book = if let Some(document) = epub.as_mut() {
            document.normalize_standard_entities()?;
            warnings.push(
                "Native reflow conversion flattens source layout, CSS, fonts and nested navigation"
                    .into(),
            );
            book_from_epub(document, &mut warnings)?
        } else {
            match source_format {
                BookFormat::Txt => book_from_txt(&read_text(source)?),
                BookFormat::Html => book_from_html(&read_text(source)?, source, &mut warnings)?,
                BookFormat::Fb2 => book_from_fb2(&read_text(source)?, &mut warnings)?,
                _ => {
                    return Err(AppError::Unsupported(
                        "Unsupported conversion source".into(),
                    ));
                }
            }
        };
        if book.sections.is_empty()
            || book
                .sections
                .iter()
                .all(|section| section.blocks.is_empty())
        {
            return Err(invalid("Source has no readable text or supported images"));
        }
        book.cover = book.cover.or_else(|| book.resources.keys().next().cloned());
        let image_count = book.resources.len() as u64;
        match target {
            BookFormat::Epub => build_epub(&book, &metadata)?.write(destination, 9)?,
            BookFormat::Txt => {
                warnings.push("TXT cannot embed book metadata, images or rich formatting".into());
                write_exclusive(destination, render_txt(&book).as_bytes())?;
            }
            BookFormat::Html => {
                write_exclusive(destination, render_html(&book, &metadata).as_bytes())?
            }
            BookFormat::Fb2 => {
                if metadata
                    .series_index
                    .is_some_and(|index| index.fract() != 0.0)
                {
                    warnings.push("FB2 only supports integer series positions; the fractional position remains in the library metadata and is omitted from this export".into());
                }
                write_exclusive(destination, render_fb2(&book, &metadata).as_bytes())?
            }
            BookFormat::Mobi => {
                warnings.push("MOBI6 uses chapter page breaks and cannot preserve EPUB CSS, fonts or series metadata portably".into());
                let bytes = write_mobi6(&book, &metadata)?;
                validate_native_mobi(&bytes)?;
                if self.engine_available() {
                    let verification = staging.path.join("verify.mobi");
                    write_exclusive(&verification, &bytes)?;
                    let reconstructed =
                        self.reconstruct_mobi(&verification, &staging, &mut warnings)?;
                    let output_text = reconstructed.spine_texts()?.join(" ");
                    if normalized_text(&output_text) != normalized_text(&render_txt(&book)) {
                        return Err(invalid(
                            "MOBI verification detected a change in reading text",
                        ));
                    }
                }
                write_exclusive(destination, &bytes)?;
            }
            _ => {
                return Err(AppError::Unsupported(
                    "Unsupported conversion target".into(),
                ));
            }
        }
        warnings.sort();
        warnings.dedup();
        Ok(ConversionReport {
            source_format,
            target_format: target,
            before_bytes: source_metadata.len(),
            after_bytes: fs::metadata(destination)?.len(),
            section_count: book.sections.len() as u64,
            images_preserved: if target == BookFormat::Txt {
                0
            } else {
                image_count
            },
            warnings,
        })
    }

    fn engine_available(&self) -> bool {
        fs::symlink_metadata(&self.mobitool_path).is_ok_and(|metadata| {
            metadata.file_type().is_file() && metadata.permissions().mode() & 0o111 != 0
        })
    }

    fn reconstruct_mobi(
        &self,
        source: &Path,
        staging: &StagingDirectory,
        warnings: &mut Vec<String>,
    ) -> Result<EpubDocument> {
        if !self.engine_available() {
            return Err(AppError::Unsupported(
                "The bundled MOBI input engine is unavailable".into(),
            ));
        }
        let run = staging.path.join(Uuid::new_v4().to_string());
        fs::DirBuilder::new().mode(0o700).create(&run)?;
        let input = run.join("input.mobi");
        copy_bounded(source, &input, MAX_SOURCE_BYTES)?;
        let engine = fs::canonicalize(&self.mobitool_path)?;
        run_mobitool(&engine, &input, &run)?;
        let output = run.join("input.epub");
        if !fs::symlink_metadata(&output).is_ok_and(|metadata| {
            metadata.file_type().is_file() && metadata.len() <= MAX_OUTPUT_BYTES
        }) {
            return Err(invalid("MOBI engine did not produce a bounded EPUB"));
        }
        let mut document = EpubDocument::open(&output)?;
        document.normalize_standard_entities()?;
        // libmobi can emit HTML5 chapters inside its EPUB container. Rebuild
        // valid, inactive XHTML instead of exposing that intermediate archive.
        let book = book_from_epub(&document, warnings)?;
        build_epub(&book, &document.metadata)
    }
}

#[derive(Debug, Clone)]
enum TextKind {
    Paragraph,
    Heading(u8),
    Preformatted,
}

#[derive(Debug, Clone)]
enum Block {
    Text { text: String, kind: TextKind },
    Image { resource: String, alt: String },
}

#[derive(Debug, Clone)]
struct Section {
    title: String,
    blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
struct Resource {
    media_type: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
struct NativeBook {
    sections: Vec<Section>,
    resources: BTreeMap<String, Resource>,
    cover: Option<String>,
}

fn invalid(message: &str) -> AppError {
    AppError::InvalidInput(message.into())
}

fn source_format(path: &Path) -> Result<BookFormat> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "epub" => Ok(BookFormat::Epub),
        "txt" => Ok(BookFormat::Txt),
        "html" | "htm" | "xhtml" => Ok(BookFormat::Html),
        "fb2" => Ok(BookFormat::Fb2),
        "mobi" | "prc" => Ok(BookFormat::Mobi),
        "azw3" | "azw" | "kf8" => Ok(BookFormat::Azw3),
        "pdf" => Ok(BookFormat::Pdf),
        "cbz" => Ok(BookFormat::Cbz),
        _ => Err(AppError::Unsupported(
            "The source extension has no supported native converter".into(),
        )),
    }
}

fn validate_destination(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(AppError::Conflict(
            "Conversion destination already exists".into(),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn read_bytes(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        return Err(invalid(
            "Input exceeds conversion limits or is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(invalid("Input exceeds conversion limits"));
    }
    Ok(bytes)
}

fn read_text(path: &Path) -> Result<String> {
    let bytes = read_bytes(path, MAX_TEXT_BYTES)?;
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| {
        AppError::Unsupported("Text conversion requires UTF-8, optionally with a BOM".into())
    })?;
    if text
        .chars()
        .any(|character| character < ' ' && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(invalid("Text input contains invalid control characters"));
    }
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn xml(source: &str) -> Result<Document<'_>> {
    Document::parse_with_options(
        source,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|_| invalid("Conversion XML is not well formed"))
}

fn book_from_txt(text: &str) -> NativeBook {
    let mut blocks = Vec::new();
    let mut lines = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !lines.is_empty() {
                blocks.push(Block::Text {
                    text: lines.join("\n"),
                    kind: TextKind::Paragraph,
                });
                lines.clear();
            }
        } else {
            lines.push(line);
        }
    }
    if !lines.is_empty() {
        blocks.push(Block::Text {
            text: lines.join("\n"),
            kind: TextKind::Paragraph,
        });
    }
    NativeBook {
        sections: vec![Section {
            title: "Text".into(),
            blocks,
        }],
        ..Default::default()
    }
}

fn normalized_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn block_kind(node: Node<'_, '_>) -> Option<TextKind> {
    if !node.is_element() {
        return None;
    }
    match node.tag_name().name() {
        "h1" => Some(TextKind::Heading(1)),
        "h2" => Some(TextKind::Heading(2)),
        "h3" => Some(TextKind::Heading(3)),
        "h4" => Some(TextKind::Heading(4)),
        "h5" => Some(TextKind::Heading(5)),
        "h6" => Some(TextKind::Heading(6)),
        "pre" => Some(TextKind::Preformatted),
        "p" | "li" | "blockquote" | "figcaption" | "td" | "th" | "v" | "subtitle" => {
            Some(TextKind::Paragraph)
        }
        _ => None,
    }
}

fn markup_blocks<F>(root: Node<'_, '_>, mut image: F) -> Result<Vec<Block>>
where
    F: FnMut(Node<'_, '_>) -> Result<Option<(String, String)>>,
{
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut group = None;
    let mut kind = TextKind::Paragraph;
    let mut total = 0_u64;
    for node in root.descendants() {
        let ancestors: Vec<_> = node.ancestors().take(MAX_MARKUP_DEPTH + 1).collect();
        if ancestors.len() > MAX_MARKUP_DEPTH {
            return Err(invalid("Markup nesting exceeds conversion limits"));
        }
        if ancestors.iter().any(|ancestor| {
            ancestor.is_element()
                && matches!(ancestor.tag_name().name(), "head" | "script" | "style")
        }) {
            continue;
        }
        if node.is_element() && matches!(node.tag_name().name(), "img" | "image") {
            if let Some((resource, alt)) = image(node)? {
                flush_block(&mut blocks, &mut current, &kind);
                blocks.push(Block::Image { resource, alt });
                group = None;
            }
        } else if node.is_element() && matches!(node.tag_name().name(), "br" | "empty-line") {
            flush_block(&mut blocks, &mut current, &kind);
            group = None;
        } else if node.is_text() {
            let parent = ancestors
                .iter()
                .find(|ancestor| block_kind(**ancestor).is_some())
                .copied()
                .unwrap_or(root);
            let next_group = parent.range().start;
            if group != Some(next_group) {
                flush_block(&mut blocks, &mut current, &kind);
                group = Some(next_group);
                kind = block_kind(parent).unwrap_or(TextKind::Paragraph);
            }
            let text = node.text().unwrap_or_default();
            total = total
                .checked_add(text.len() as u64)
                .ok_or_else(|| invalid("Text size overflow"))?;
            if total > MAX_TEXT_BYTES {
                return Err(invalid("Readable text exceeds conversion limits"));
            }
            current.push_str(text);
        }
    }
    flush_block(&mut blocks, &mut current, &kind);
    Ok(blocks)
}

fn flush_block(blocks: &mut Vec<Block>, current: &mut String, kind: &TextKind) {
    let text = if matches!(kind, TextKind::Preformatted) {
        current.trim_matches('\n').to_owned()
    } else {
        normalized_text(current)
    };
    current.clear();
    if !text.trim().is_empty() {
        blocks.push(Block::Text {
            text,
            kind: kind.clone(),
        });
    }
}

fn section_title(blocks: &[Block], fallback: &str) -> String {
    blocks
        .iter()
        .find_map(|block| match block {
            Block::Text {
                text,
                kind: TextKind::Heading(_),
            } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_else(|| fallback.into())
}

fn add_resource(book: &mut NativeBook, bytes: Vec<u8>, media_type: &str) -> Result<String> {
    let expected = match media_type {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        _ => {
            return Err(AppError::Unsupported(
                "Native conversion embeds only JPEG and PNG images".into(),
            ));
        }
    };
    if bytes.len() as u64 > MAX_ASSET_ENTRY_BYTES {
        return Err(invalid("Embedded image exceeds conversion limits"));
    }
    let mut reader = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|_| invalid("Invalid image header"))?;
    if reader.format() != Some(expected) {
        return Err(invalid("Image signature and declared media type disagree"));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(16_000_000);
    limits.max_image_height = Some(16_000_000);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| invalid("Invalid image dimensions"))?;
    if u64::from(width)
        .checked_mul(u64::from(height))
        .is_none_or(|pixels| pixels == 0 || pixels > 16_000_000)
    {
        return Err(invalid("Embedded image exceeds the pixel limit"));
    }
    if let Some((key, _)) = book
        .resources
        .iter()
        .find(|(_, resource)| resource.bytes == bytes)
    {
        return Ok(key.clone());
    }
    let total = book
        .resources
        .values()
        .map(|resource| resource.bytes.len() as u64)
        .sum::<u64>()
        + bytes.len() as u64;
    if total > MAX_ASSET_BYTES {
        return Err(invalid("Embedded images exceed conversion limits"));
    }
    let key = format!(
        "images/image-{}.{}",
        book.resources.len() + 1,
        if expected == ImageFormat::Jpeg {
            "jpg"
        } else {
            "png"
        }
    );
    book.resources.insert(
        key.clone(),
        Resource {
            media_type: media_type.into(),
            bytes,
        },
    );
    Ok(key)
}

fn book_from_epub(document: &EpubDocument, warnings: &mut Vec<String>) -> Result<NativeBook> {
    let mut book = NativeBook::default();
    let mut resources = HashMap::new();
    for item in &document.manifest {
        if matches!(item.media_type.as_str(), "image/jpeg" | "image/png") {
            let key = add_resource(
                &mut book,
                document.entries[&item.href].clone(),
                &item.media_type,
            )?;
            resources.insert(item.href.clone(), key);
        } else if item.media_type.starts_with("image/") {
            warnings.push("Vector or unsupported image resources are omitted during native reflow conversion; visible text is retained".into());
        }
    }
    book.cover = document
        .cover_path
        .as_ref()
        .and_then(|path| resources.get(path))
        .cloned();
    let mut total_text = 0_u64;
    for (index, path) in document.spine.iter().enumerate() {
        let source = std::str::from_utf8(&document.entries[path]).map_err(|_| {
            AppError::Unsupported(
                "EPUB chapters must use UTF-8 for native reflow conversion".into(),
            )
        })?;
        let normalized;
        let parsed = match crate::epub::parse_markup(source) {
            Ok(parsed) => parsed,
            Err(_) => {
                // Inspection rejects internal DTD/entity declarations before
                // the HTML5 sanitizer is allowed to interpret tolerant markup.
                crate::epub::text_for_inspection(source)?;
                normalized = safe_html_xml(source)?;
                warnings.push("A chapter used HTML5 normalization; active content and unsupported formatting were removed".into());
                xml(&normalized)?
            }
        };
        let root = parsed
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "body")
            .unwrap_or(parsed.root_element());
        let blocks = markup_blocks(root, |node| {
            let src = node
                .attribute("src")
                .or_else(|| node.attribute((XLINK, "href")))
                .unwrap_or_default();
            Ok(resolve_href(path, src)
                .ok()
                .and_then(|path| resources.get(&path))
                .map(|resource| {
                    (
                        resource.clone(),
                        node.attribute("alt").unwrap_or_default().into(),
                    )
                }))
        })?;
        total_text += blocks
            .iter()
            .map(|block| match block {
                Block::Text { text, .. } => text.len() as u64,
                _ => 0,
            })
            .sum::<u64>();
        if total_text > MAX_TEXT_BYTES {
            return Err(invalid("Readable text exceeds conversion limits"));
        }
        book.sections.push(Section {
            title: section_title(&blocks, &format!("Section {}", index + 1)),
            blocks,
        });
    }
    Ok(book)
}

fn safe_html_xml(source: &str) -> Result<String> {
    let sanitized = ammonia::Builder::default()
        .add_clean_content_tags(&["title"])
        .generic_attributes(HashSet::new())
        .tag_attributes(HashMap::from([("img", HashSet::from(["src", "alt"]))]))
        .url_schemes(HashSet::from(["data"]))
        .link_rel(None)
        .clean(source)
        .to_string()
        .replace("&nbsp;", "&#160;");
    let bytes = sanitized.as_bytes();
    let mut output = String::with_capacity(sanitized.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'<' {
            let next = sanitized[index..]
                .find('<')
                .map(|offset| index + offset)
                .unwrap_or(bytes.len());
            output.push_str(&sanitized[index..next]);
            index = next;
            continue;
        }
        let start = index;
        let mut quote = None;
        index += 1;
        while index < bytes.len() {
            let byte = bytes[index];
            if let Some(delimiter) = quote {
                if byte == delimiter {
                    quote = None;
                }
            } else if matches!(byte, b'\'' | b'"') {
                quote = Some(byte);
            } else if byte == b'>' {
                break;
            }
            index += 1;
        }
        if index >= bytes.len() {
            return Err(invalid("Sanitized HTML has an incomplete element"));
        }
        let tag = &sanitized[start + 1..index];
        let name = tag
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default()
            .trim_end_matches('/');
        output.push_str(&sanitized[start..index]);
        if matches!(name, "area" | "br" | "col" | "hr" | "img" | "wbr") && !tag.ends_with('/') {
            output.push('/');
        }
        output.push('>');
        index += 1;
    }
    Ok(format!(
        "<html xmlns=\"{XHTML}\"><body>{output}</body></html>"
    ))
}

fn book_from_html(source: &str, path: &Path, warnings: &mut Vec<String>) -> Result<NativeBook> {
    let sanitized = safe_html_xml(source)?;
    let parsed = xml(&sanitized)?;
    let mut book = NativeBook::default();
    let base = fs::canonicalize(path.parent().unwrap_or_else(|| Path::new(".")))?;
    let blocks = markup_blocks(parsed.root_element(), |node| {
        let src = node.attribute("src").unwrap_or_default();
        let resource = if let Some(data) = src.strip_prefix("data:") {
            let (header, data) = data
                .split_once(',')
                .ok_or_else(|| invalid("Invalid embedded image URI"))?;
            let mime = header
                .strip_suffix(";base64")
                .ok_or_else(|| invalid("Embedded images require base64 encoding"))?;
            if !matches!(mime, "image/jpeg" | "image/png") {
                None
            } else {
                Some(add_resource(
                    &mut book,
                    STANDARD
                        .decode(data)
                        .map_err(|_| invalid("Invalid embedded image base64"))?,
                    mime,
                )?)
            }
        } else if let Ok(relative) = resolve_href("source.html", src) {
            let candidate = base.join(relative);
            let canonical = fs::canonicalize(&candidate);
            if let Ok(canonical) = canonical
                && canonical.starts_with(&base)
                && fs::symlink_metadata(&candidate)?.file_type().is_file()
            {
                let bytes = read_bytes(&candidate, MAX_ASSET_ENTRY_BYTES)?;
                let mime = match image::guess_format(&bytes) {
                    Ok(ImageFormat::Jpeg) => "image/jpeg",
                    Ok(ImageFormat::Png) => "image/png",
                    _ => "unsupported",
                };
                if mime == "unsupported" {
                    None
                } else {
                    Some(add_resource(&mut book, bytes, mime)?)
                }
            } else {
                None
            }
        } else {
            None
        };
        if resource.is_none() {
            warnings.push("An external, missing or unsupported HTML image was omitted; no network request was made".into());
        }
        Ok(resource.map(|resource| (resource, node.attribute("alt").unwrap_or_default().into())))
    })?;
    book.sections.push(Section {
        title: section_title(&blocks, "HTML"),
        blocks,
    });
    warnings.push("HTML scripts, styles, remote resources and interactive features are excluded from native reflow conversion".into());
    Ok(book)
}

fn book_from_fb2(source: &str, warnings: &mut Vec<String>) -> Result<NativeBook> {
    let parsed = xml(source)?;
    if !parsed.root_element().has_tag_name((FB2, "FictionBook")) {
        return Err(invalid("FB2 root namespace is invalid"));
    }
    let mut book = NativeBook::default();
    let mut resources = HashMap::new();
    for binary in parsed
        .root_element()
        .children()
        .filter(|node| node.has_tag_name((FB2, "binary")))
    {
        let id = binary
            .attribute("id")
            .ok_or_else(|| invalid("FB2 binary has no ID"))?;
        if resources.contains_key(id) {
            return Err(invalid("FB2 binary IDs are duplicated"));
        }
        let mime = binary.attribute("content-type").unwrap_or_default();
        if !matches!(mime, "image/jpeg" | "image/png") {
            warnings.push("Unsupported FB2 binary resources are omitted".into());
            continue;
        }
        let encoded: String = binary
            .text()
            .unwrap_or_default()
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect();
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| invalid("Invalid FB2 binary base64"))?;
        let key = add_resource(&mut book, bytes, mime)?;
        resources.insert(id.to_owned(), key);
    }
    book.cover = parsed
        .descendants()
        .find(|node| node.has_tag_name((FB2, "coverpage")))
        .and_then(|node| {
            node.descendants()
                .find(|child| child.has_tag_name((FB2, "image")))
        })
        .and_then(|node| node.attribute((XLINK, "href")))
        .and_then(|href| href.strip_prefix('#'))
        .and_then(|id| resources.get(id))
        .cloned();
    for body in parsed
        .root_element()
        .children()
        .filter(|node| node.has_tag_name((FB2, "body")))
    {
        let sections: Vec<_> = body
            .children()
            .filter(|node| node.has_tag_name((FB2, "section")))
            .collect();
        let roots = if sections.is_empty() {
            vec![body]
        } else {
            sections
        };
        for root in roots {
            let blocks = markup_blocks(root, |node| {
                Ok(node
                    .attribute((XLINK, "href"))
                    .and_then(|href| href.strip_prefix('#'))
                    .and_then(|id| resources.get(id))
                    .map(|resource| (resource.clone(), String::new())))
            })?;
            book.sections.push(Section {
                title: section_title(&blocks, &format!("Section {}", book.sections.len() + 1)),
                blocks,
            });
        }
    }
    if book.sections.is_empty() {
        return Err(invalid("FB2 has no reading body"));
    }
    Ok(book)
}

fn render_blocks<F>(section: &Section, mut image: F) -> String
where
    F: FnMut(&str, &str) -> String,
{
    section
        .blocks
        .iter()
        .map(|block| match block {
            Block::Text { text, kind } => {
                let tag = match kind {
                    TextKind::Paragraph => "p".into(),
                    TextKind::Heading(level) => format!("h{level}"),
                    TextKind::Preformatted => "pre".into(),
                };
                format!("<{tag}>{}</{tag}>\n", escape_xml(text))
            }
            Block::Image { resource, alt } => image(resource, alt),
        })
        .collect()
}

fn build_epub(book: &NativeBook, metadata: &BookMetadata) -> Result<EpubDocument> {
    let mut entries = BTreeMap::from([
        ("mimetype".into(), b"application/epub+zip".to_vec()),
        ("META-INF/container.xml".into(), br#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_vec()),
        ("OPS/style.css".into(), b"body{line-height:1.5}img{max-width:100%;height:auto}pre{white-space:pre-wrap}".to_vec()),
    ]);
    let mut manifest = String::from(
        r#"<item id="style" href="style.css" media-type="text/css"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>"#,
    );
    let mut spine = String::new();
    let mut nav = String::new();
    for (index, section) in book.sections.iter().enumerate() {
        let path = format!("sections/section-{}.xhtml", index + 1);
        let blocks = render_blocks(section, |resource, alt| {
            format!(
                "<img src=\"../{}\" alt=\"{}\"/>\n",
                escape_xml(resource),
                escape_xml(alt)
            )
        });
        entries.insert(format!("OPS/{path}"), format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><html xmlns=\"{XHTML}\" lang=\"{}\"><head><title>{}</title><link rel=\"stylesheet\" href=\"../style.css\"/></head><body>{blocks}</body></html>", escape_xml(&metadata.language), escape_xml(&section.title)).into_bytes());
        manifest.push_str(&format!(
            "<item id=\"s{}\" href=\"{path}\" media-type=\"application/xhtml+xml\"/>",
            index + 1
        ));
        spine.push_str(&format!("<itemref idref=\"s{}\"/>", index + 1));
        nav.push_str(&format!(
            "<li><a href=\"{path}\">{}</a></li>",
            escape_xml(&section.title)
        ));
    }
    for (index, (path, resource)) in book.resources.iter().enumerate() {
        entries.insert(format!("OPS/{path}"), resource.bytes.clone());
        manifest.push_str(&format!(
            "<item id=\"image{}\" href=\"{path}\" media-type=\"{}\"{}/>",
            index + 1,
            resource.media_type,
            if book.cover.as_ref() == Some(path) {
                " properties=\"cover-image\""
            } else {
                ""
            }
        ));
    }
    entries.insert("OPS/nav.xhtml".into(), format!("<html xmlns=\"{XHTML}\" xmlns:epub=\"http://www.idpf.org/2007/ops\"><head><title>{}</title></head><body><nav epub:type=\"toc\"><ol>{nav}</ol></nav></body></html>", escape_xml(&metadata.title)).into_bytes());
    entries.insert("OPS/content.opf".into(), format!("<package xmlns=\"http://www.idpf.org/2007/opf\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" version=\"3.0\" unique-identifier=\"uid\"><metadata><dc:identifier id=\"uid\">urn:uuid:{}</dc:identifier><dc:title>{}</dc:title><dc:language>{}</dc:language></metadata><manifest>{manifest}</manifest><spine>{spine}</spine></package>", Uuid::new_v4(), escape_xml(&metadata.title), escape_xml(&metadata.language)).into_bytes());
    let mut document = EpubDocument::from_entries(entries, &metadata.title)?;
    document.update_metadata(metadata)?;
    document.text_fingerprint()?;
    Ok(document)
}

fn render_txt(book: &NativeBook) -> String {
    let mut paragraphs = Vec::new();
    for section in &book.sections {
        for block in &section.blocks {
            if let Block::Text { text, .. } = block {
                paragraphs.push(text.as_str());
            }
        }
    }
    paragraphs.join("\n\n") + "\n"
}

fn render_html(book: &NativeBook, metadata: &BookMetadata) -> String {
    let sections: String = book
        .sections
        .iter()
        .map(|section| {
            format!(
                "<section>{}</section>",
                render_blocks(section, |resource, alt| {
                    let image = &book.resources[resource];
                    format!(
                        "<img src=\"data:{};base64,{}\" alt=\"{}\"/>\n",
                        image.media_type,
                        STANDARD.encode(&image.bytes),
                        escape_xml(alt)
                    )
                })
            )
        })
        .collect();
    format!(
        "<!DOCTYPE html><html lang=\"{}\"><head><meta charset=\"UTF-8\"/><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'\"/><title>{}</title><style>body{{max-width:48rem;margin:auto;padding:2rem;line-height:1.6}}img{{max-width:100%}}pre{{white-space:pre-wrap}}</style></head><body>{sections}</body></html>",
        escape_xml(&metadata.language),
        escape_xml(&metadata.title)
    )
}

fn render_fb2(book: &NativeBook, metadata: &BookMetadata) -> String {
    let resource_ids: HashMap<_, _> = book
        .resources
        .keys()
        .enumerate()
        .map(|(index, key)| (key.as_str(), format!("image{}", index + 1)))
        .collect();
    let mut authors: String = metadata
        .authors
        .iter()
        .map(|author| {
            format!(
                "<author><nickname>{}</nickname></author>",
                escape_xml(author)
            )
        })
        .collect();
    if authors.is_empty() {
        authors = "<author><nickname/></author>".into();
    }
    let genres: String = metadata
        .genres
        .iter()
        .map(|genre| format!("<genre>{}</genre>", fb2_genre(genre)))
        .collect();
    let genres = if genres.is_empty() {
        "<genre>unrecognised</genre>".into()
    } else {
        genres
    };
    let keywords = metadata
        .genres
        .iter()
        .chain(&metadata.tags)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let keywords = if keywords.is_empty() {
        String::new()
    } else {
        format!("<keywords>{}</keywords>", escape_xml(&keywords))
    };
    let annotation = if metadata.description.is_empty() {
        String::new()
    } else {
        format!(
            "<annotation><p>{}</p></annotation>",
            escape_xml(&metadata.description)
        )
    };
    let sequence = metadata
        .series
        .as_ref()
        .map(|series| {
            format!(
                "<sequence name=\"{}\"{}/>",
                escape_xml(series),
                metadata
                    .series_index
                    .filter(|index| index.fract() == 0.0)
                    .map(|index| format!(" number=\"{index}\""))
                    .unwrap_or_default()
            )
        })
        .unwrap_or_default();
    let cover = book
        .cover
        .as_ref()
        .and_then(|path| resource_ids.get(path.as_str()))
        .map(|id| format!("<coverpage><image l:href=\"#{id}\"/></coverpage>"))
        .unwrap_or_default();
    let body: String = book
        .sections
        .iter()
        .map(|section| {
            let blocks: String = section
                .blocks
                .iter()
                .map(|block| match block {
                    Block::Text {
                        text,
                        kind: TextKind::Heading(_),
                    } => format!("<subtitle>{}</subtitle>", escape_xml(text)),
                    Block::Text { text, .. } => format!("<p>{}</p>", escape_xml(text)),
                    Block::Image { resource, .. } => {
                        format!("<image l:href=\"#{}\"/>", resource_ids[resource.as_str()])
                    }
                })
                .collect();
            format!("<section>{blocks}</section>")
        })
        .collect();
    let binaries: String = book
        .resources
        .iter()
        .map(|(path, resource)| {
            format!(
                "<binary id=\"{}\" content-type=\"{}\">{}</binary>",
                resource_ids[path.as_str()],
                resource.media_type,
                STANDARD.encode(&resource.bytes)
            )
        })
        .collect();
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><FictionBook xmlns=\"{FB2}\" xmlns:l=\"{XLINK}\"><description><title-info>{genres}{authors}<book-title>{}</book-title>{annotation}{keywords}{cover}<lang>{}</lang>{sequence}</title-info><document-info><author><nickname>Library Manager</nickname></author><date value=\"{date}\">{date}</date><id>{}</id><version>1.0</version></document-info></description><body>{body}</body>{binaries}</FictionBook>",
        escape_xml(&metadata.title),
        escape_xml(&metadata.language),
        Uuid::new_v4()
    )
}

fn fb2_genre(genre: &str) -> &'static str {
    match genre.trim().to_lowercase().as_str() {
        "fantasy" | "fantastique" | "sf_fantasy" => "sf_fantasy",
        "science-fiction" | "science fiction" | "sf" => "sf",
        "policier" | "detective" => "detective",
        "thriller" => "thriller",
        "roman" | "fiction" | "narrative" => "narrative",
        "poésie" | "poetry" => "poetry",
        "biographie" | "biography" | "nonf_biography" => "nonf_biography",
        "nonfiction" => "nonfiction",
        "classique" | "classic" | "prose_classic" => "prose_classic",
        "romance" | "love_contemporary" => "love_contemporary",
        "adventure" | "aventure" => "adventure",
        "children" | "jeunesse" => "children",
        "reference" | "référence" => "reference",
        "science" => "science",
        "computers" | "informatique" => "computers",
        _ => "unrecognised",
    }
}

fn validate_metadata(metadata: &BookMetadata) -> Result<()> {
    if metadata.title.len() > 4096
        || metadata.language.len() > 64
        || metadata.description.len() > 65_536
        || metadata.authors.len() > 128
        || metadata.genres.len() > 128
        || metadata.tags.len() > 128
        || metadata
            .series_index
            .is_some_and(|index| !index.is_finite() || index < 0.0)
    {
        return Err(invalid("Conversion metadata exceeds field limits"));
    }
    let fields = [
        metadata.title.as_str(),
        metadata.language.as_str(),
        metadata.description.as_str(),
        metadata.author_sort.as_str(),
    ]
    .into_iter()
    .chain(metadata.authors.iter().map(String::as_str))
    .chain(metadata.genres.iter().map(String::as_str))
    .chain(metadata.tags.iter().map(String::as_str))
    .chain(metadata.series.as_deref())
    .chain(metadata.publisher.as_deref())
    .chain(metadata.isbn.as_deref())
    .chain(metadata.published.as_deref());
    for field in fields {
        if field.len() > 65_536
            || field
                .chars()
                .any(|character| character < ' ' && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(invalid(
                "Conversion metadata contains invalid or oversized text",
            ));
        }
    }
    Ok(())
}

fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 > MAX_OUTPUT_BYTES {
        return Err(invalid("Converted output exceeds the safety limit"));
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let result = file.write_all(bytes).and_then(|_| file.sync_all());
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result.map_err(Into::into)
}

fn copy_bounded(source: &Path, destination: &Path, maximum: u64) -> Result<()> {
    let mut input = File::open(source)?.take(maximum + 1);
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let copied = std::io::copy(&mut input, &mut output)?;
    if copied > maximum {
        return Err(invalid("MOBI source exceeds conversion limits"));
    }
    output.sync_all()?;
    Ok(())
}

struct StagingDirectory {
    path: PathBuf,
}

impl StagingDirectory {
    fn new() -> Result<Self> {
        let path = Path::new("/tmp").join(format!("library-manager-convert-{}", Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self { path })
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct RunningChild {
    child: Child,
    completed: bool,
}
impl Drop for RunningChild {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn drain_child<R: Read + Send + 'static>(
    mut pipe: R,
    exceeded: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        let mut total = 0;
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    total += count;
                    if total > MAX_CHILD_LOG_BYTES {
                        exceeded.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
        }
    })
}

fn run_mobitool(engine: &Path, source: &Path, directory: &Path) -> Result<()> {
    let child = Command::new(engine)
        .arg("-e")
        .arg("-o")
        .arg(directory)
        .arg(source)
        .current_dir(directory)
        .env_clear()
        .env("LC_ALL", "C.UTF-8")
        .env("TMPDIR", directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut running = RunningChild {
        child,
        completed: false,
    };
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout = drain_child(
        running
            .child
            .stdout
            .take()
            .ok_or_else(|| invalid("MOBI process stdout is unavailable"))?,
        exceeded.clone(),
    );
    let stderr = drain_child(
        running
            .child
            .stderr
            .take()
            .ok_or_else(|| invalid("MOBI process stderr is unavailable"))?,
        exceeded.clone(),
    );
    let started = Instant::now();
    let result = loop {
        if exceeded.load(Ordering::Relaxed) {
            break Err(invalid("MOBI engine exceeded bounded diagnostic output"));
        }
        if started.elapsed() >= MOBI_TIMEOUT {
            break Err(AppError::Unsupported(
                "MOBI engine conversion exceeded its 60-second limit".into(),
            ));
        }
        match running.child.try_wait() {
            Ok(Some(status)) => {
                running.completed = true;
                break if status.success() {
                    Ok(())
                } else {
                    Err(AppError::Unsupported(
                        "MOBI engine could not reconstruct this unencrypted book".into(),
                    ))
                };
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => break Err(error.into()),
        }
    };
    if !running.completed {
        let _ = running.child.kill();
        let _ = running.child.wait();
        running.completed = true;
    }
    let _ = stdout.join();
    let _ = stderr.join();
    if exceeded.load(Ordering::Relaxed) {
        return Err(invalid("MOBI engine exceeded bounded diagnostic output"));
    }
    result
}

fn append_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}
fn append_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}
fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn write_mobi6(book: &NativeBook, metadata: &BookMetadata) -> Result<Vec<u8>> {
    let images: Vec<_> = book.resources.iter().collect();
    let image_indices: HashMap<_, _> = images
        .iter()
        .enumerate()
        .map(|(index, (path, _))| (path.as_str(), index + 1))
        .collect();
    let mut html = format!(
        "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\"/><title>{}</title></head><body>",
        escape_xml(&metadata.title)
    );
    for (index, section) in book.sections.iter().enumerate() {
        if index > 0 {
            html.push_str("<mbp:pagebreak/>");
        }
        html.push_str(&format!("<div id=\"lm-section-{index}\">"));
        html.push_str(&render_blocks(section, |resource, alt| {
            format!(
                "<img recindex=\"{:05}\" alt=\"{}\"/>",
                image_indices[resource],
                escape_xml(alt)
            )
        }));
        html.push_str("</div>");
    }
    html.push_str("</body></html>");
    if html.len() as u64 > MAX_TEXT_BYTES * 2 {
        return Err(invalid("MOBI text exceeds conversion limits"));
    }
    let mut text_records = Vec::new();
    let mut start = 0;
    while start < html.len() {
        let mut end = (start + PALMDOC_RECORD_BYTES).min(html.len());
        while !html.is_char_boundary(end) {
            end -= 1;
        }
        text_records.push(html.as_bytes()[start..end].to_vec());
        start = end;
    }
    let text_count =
        u16::try_from(text_records.len()).map_err(|_| invalid("MOBI has too many text records"))?;
    let first_image = text_records.len() + 1;
    let record_count = first_image + images.len() + 1;
    let record_count_u16 =
        u16::try_from(record_count).map_err(|_| invalid("MOBI has too many records"))?;
    let mut exth_records = Vec::new();
    let mut exth = |tag: u32, value: &[u8]| {
        let mut record = Vec::new();
        append_u32(&mut record, tag);
        append_u32(&mut record, (value.len() + 8) as u32);
        record.extend_from_slice(value);
        exth_records.push(record);
    };
    for author in &metadata.authors {
        exth(100, author.as_bytes());
    }
    exth(503, metadata.title.as_bytes());
    exth(524, metadata.language.as_bytes());
    exth(501, b"PDOC");
    for (tag, value) in [
        (101, metadata.publisher.as_deref()),
        (103, Some(metadata.description.as_str())),
        (104, metadata.isbn.as_deref()),
        (106, metadata.published.as_deref()),
    ] {
        if let Some(value) = value {
            exth(tag, value.as_bytes());
        }
    }
    for genre in &metadata.genres {
        exth(105, genre.as_bytes());
    }
    if let Some(cover) = book
        .cover
        .as_ref()
        .and_then(|cover| image_indices.get(cover.as_str()))
    {
        exth(201, &((*cover - 1) as u32).to_be_bytes());
    }
    exth(125, &(images.len() as u32).to_be_bytes());
    let mut extended = b"EXTH".to_vec();
    append_u32(&mut extended, 0);
    append_u32(&mut extended, exth_records.len() as u32);
    for record in exth_records {
        extended.extend_from_slice(&record);
    }
    while !extended.len().is_multiple_of(4) {
        extended.push(0);
    }
    let extended_length = extended.len() as u32;
    set_u32(&mut extended, 4, extended_length);
    let mut header = vec![0_u8; MOBI_HEADER_BYTES];
    header[0..4].copy_from_slice(b"MOBI");
    for offset in [
        24, 28, 32, 36, 40, 44, 48, 52, 56, 60, 96, 104, 152, 184, 192, 208, 228,
    ] {
        set_u32(&mut header, offset, u32::MAX);
    }
    let digest = Sha256::digest(html.as_bytes());
    let uid = u32::from_be_bytes(
        digest[..4]
            .try_into()
            .map_err(|_| invalid("Invalid MOBI digest"))?,
    );
    for (offset, value) in [
        (4, MOBI_HEADER_BYTES as u32),
        (8, 2),
        (12, 65001),
        (16, uid),
        (20, 6),
        (64, first_image as u32),
        (68, 16 + MOBI_HEADER_BYTES as u32 + extended_length),
        (72, metadata.title.len() as u32),
        (88, 6),
        (
            92,
            if images.is_empty() {
                u32::MAX
            } else {
                first_image as u32
            },
        ),
        (112, 0x40),
    ] {
        set_u32(&mut header, offset, value);
    }
    header[176..178].copy_from_slice(&1_u16.to_be_bytes());
    header[178..180].copy_from_slice(&text_count.to_be_bytes());
    let mut record0 = Vec::new();
    append_u16(&mut record0, 1);
    append_u16(&mut record0, 0);
    append_u32(&mut record0, html.len() as u32);
    append_u16(&mut record0, text_count);
    append_u16(&mut record0, PALMDOC_RECORD_BYTES as u16);
    append_u16(&mut record0, 0);
    append_u16(&mut record0, 0);
    record0.extend_from_slice(&header);
    record0.extend_from_slice(&extended);
    record0.extend_from_slice(metadata.title.as_bytes());
    record0.extend_from_slice(&[0; 12]);
    let mut records = vec![record0];
    records.extend(text_records);
    records.extend(images.iter().map(|(_, resource)| resource.bytes.clone()));
    records.push(vec![0xE9, 0x8E, 0x0D, 0x0A]);
    let mut output = vec![0_u8; 78];
    let name: Vec<_> = metadata
        .title
        .chars()
        .take(31)
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == ' ' {
                character as u8
            } else {
                b'_'
            }
        })
        .collect();
    output[..name.len()].copy_from_slice(&name);
    let timestamp = u32::try_from(chrono::Utc::now().timestamp().saturating_add(2_082_844_800))
        .map_err(|_| invalid("Palm database timestamp is out of range"))?;
    set_u32(&mut output, 36, timestamp);
    set_u32(&mut output, 40, timestamp);
    output[60..64].copy_from_slice(b"BOOK");
    output[64..68].copy_from_slice(b"MOBI");
    set_u32(&mut output, 68, record_count as u32 + 1);
    output[76..78].copy_from_slice(&record_count_u16.to_be_bytes());
    let mut offset = 78 + record_count * 8 + 2;
    for (index, record) in records.iter().enumerate() {
        append_u32(
            &mut output,
            u32::try_from(offset).map_err(|_| invalid("MOBI offset exceeds the format limit"))?,
        );
        append_u32(&mut output, index as u32 + 1);
        offset += record.len();
    }
    output.extend_from_slice(&[0, 0]);
    for record in records {
        output.extend_from_slice(&record);
    }
    if output.len() as u64 > MAX_OUTPUT_BYTES {
        return Err(invalid("MOBI output exceeds conversion limits"));
    }
    Ok(output)
}

fn validate_native_mobi(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 100 || bytes.get(60..68) != Some(b"BOOKMOBI") {
        return Err(invalid("Native MOBI database header is invalid"));
    }
    let count = u16::from_be_bytes(
        bytes[76..78]
            .try_into()
            .map_err(|_| invalid("Native MOBI record count is invalid"))?,
    ) as usize;
    if count < 3 || 78 + count * 8 + 2 > bytes.len() {
        return Err(invalid("Native MOBI record table is invalid"));
    }
    let offsets: Vec<_> = (0..count)
        .map(|index| {
            u32::from_be_bytes(
                bytes[78 + index * 8..82 + index * 8]
                    .try_into()
                    .unwrap_or_default(),
            ) as usize
        })
        .collect();
    if offsets.windows(2).any(|pair| pair[0] >= pair[1])
        || offsets[0] != 78 + count * 8 + 2
        || offsets[count - 1] >= bytes.len()
    {
        return Err(invalid("Native MOBI record offsets are invalid"));
    }
    let record0 = &bytes[offsets[0]..offsets[1]];
    if record0.len() < 16 + MOBI_HEADER_BYTES
        || record0.get(16..20) != Some(b"MOBI")
        || record0.get(0..2) != Some(&1_u16.to_be_bytes())
        || record0.get(28..32) != Some(&65001_u32.to_be_bytes())
        || record0.get(36..40) != Some(&6_u32.to_be_bytes())
    {
        return Err(invalid("Native MOBI6 text header is invalid"));
    }
    let text_count = u16::from_be_bytes(
        record0[8..10]
            .try_into()
            .map_err(|_| invalid("Native MOBI text count is invalid"))?,
    ) as usize;
    if text_count == 0 || text_count + 1 >= count {
        return Err(invalid("Native MOBI text records are invalid"));
    }
    let expected = u32::from_be_bytes(
        record0[4..8]
            .try_into()
            .map_err(|_| invalid("Native MOBI text length is invalid"))?,
    ) as usize;
    let end = offsets[text_count + 1];
    let text = &bytes[offsets[1]..end];
    if text.len() != expected || std::str::from_utf8(text).is_err() {
        return Err(invalid("Native MOBI Unicode text length is invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use std::io::Cursor;
    use tempfile::TempDir;

    fn engine() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../src-tauri/binaries/library-manager-mobitool-x86_64-unknown-linux-gnu")
    }

    fn metadata() -> BookMetadata {
        BookMetadata {
            title: "L'été des étoiles — 日本語".into(),
            authors: vec!["Élodie Martin".into(), "Γιάννης".into()],
            language: "fr".into(),
            series: Some("Les constellations".into()),
            series_index: Some(3.5),
            genres: vec!["Fantasy".into()],
            description: "Un récit & des étoiles.".into(),
            publisher: Some("Éditions Tests".into()),
            ..BookMetadata::default()
        }
    }

    fn picture() -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_pixel(3, 2, Rgb([160, 100, 80])))
            .write_to(&mut output, ImageFormat::Png)
            .expect("test PNG");
        output.into_inner()
    }

    fn book() -> NativeBook {
        let image = "images/image-1.png".to_string();
        NativeBook {
            sections: vec![
                Section {
                    title: "Chapitre premier".into(),
                    blocks: vec![
                        Block::Text {
                            text: "Chapitre premier".into(),
                            kind: TextKind::Heading(1),
                        },
                        Block::Text {
                            text: "Café, αβγ et 日本語 : première phrase.".into(),
                            kind: TextKind::Paragraph,
                        },
                        Block::Image {
                            resource: image.clone(),
                            alt: "Une couverture".into(),
                        },
                        Block::Text {
                            text: "Deuxième paragraphe <avec> & signes.".into(),
                            kind: TextKind::Paragraph,
                        },
                    ],
                },
                Section {
                    title: "Chapitre suivant".into(),
                    blocks: vec![
                        Block::Text {
                            text: "Chapitre suivant".into(),
                            kind: TextKind::Heading(2),
                        },
                        Block::Text {
                            text: "Dernière phrase, bien après l'image.".into(),
                            kind: TextKind::Paragraph,
                        },
                    ],
                },
            ],
            resources: BTreeMap::from([(
                image.clone(),
                Resource {
                    media_type: "image/png".into(),
                    bytes: picture(),
                },
            )]),
            cover: Some(image),
        }
    }

    fn assert_text_order(text: &str) {
        let phrases = [
            "Chapitre premier",
            "Café, αβγ et 日本語",
            "Deuxième paragraphe",
            "Chapitre suivant",
            "Dernière phrase",
        ];
        let mut after = 0;
        for phrase in phrases {
            let position = text[after..]
                .find(phrase)
                .expect("reading text must be retained");
            after += position + phrase.len();
        }
    }

    #[test]
    fn txt_bom_roundtrip_preserves_paragraphs_unicode_metadata_and_source() {
        let directory = TempDir::new().unwrap();
        let source = directory.path().join("source.txt");
        let input = "\u{feff}Café αβγ 日本語\r\nune suite de ligne\r\n\r\nParagraphe second.\r\n";
        fs::write(&source, input).unwrap();
        let converter = Converter::new(PathBuf::from("/missing-engine"));
        let epub = directory.path().join("output.epub");
        converter
            .convert(&source, &epub, BookFormat::Epub, &metadata())
            .unwrap();
        let document = EpubDocument::open(&epub).unwrap();
        assert_eq!(document.metadata.series_index, Some(3.5));
        assert_eq!(document.metadata.authors, metadata().authors);
        let text = directory.path().join("output.txt");
        converter
            .convert(&epub, &text, BookFormat::Txt, &metadata())
            .unwrap();
        assert_eq!(
            fs::read_to_string(&text).unwrap(),
            "Café αβγ 日本語 une suite de ligne\n\nParagraphe second.\n"
        );
        assert_eq!(fs::read_to_string(&source).unwrap(), input);
        assert!(matches!(
            converter.convert(&source, &epub, BookFormat::Epub, &metadata()),
            Err(AppError::Conflict(_))
        ));
    }

    #[test]
    fn html_sanitization_preserves_inline_text_and_only_local_safe_images() {
        let directory = TempDir::new().unwrap();
        fs::write(directory.path().join("cover.png"), picture()).unwrap();
        let source = directory.path().join("source.html");
        let input = "<!doctype html><html><head><title>Not body</title><style>hidden text</style></head><body><h1>Titre</h1><p>co<em>de</em>base&nbsp;français<br>Suite</p><img src='cover.png' alt='couverture' onerror='bad()'><img src='https://invalid.example/never.png'><script>private script text</script><p>Dernier paragraphe.</p></body></html>";
        fs::write(&source, input).unwrap();
        let destination = directory.path().join("result.epub");
        let report = Converter::new(PathBuf::from("/missing-engine"))
            .convert(&source, &destination, BookFormat::Epub, &metadata())
            .unwrap();
        assert_eq!(report.images_preserved, 1);
        let document = EpubDocument::open(&destination).unwrap();
        let text = document.spine_texts().unwrap().join(" ");
        assert!(text.contains("codebase français"));
        assert!(text.contains("Suite"));
        assert!(text.contains("Dernier paragraphe"));
        assert!(!text.contains("hidden text"));
        assert!(!text.contains("private script text"));
        assert!(!text.contains("Not body"));
        for path in &document.spine {
            let chapter = String::from_utf8_lossy(&document.entries[path]);
            assert!(!chapter.contains("onerror"));
            assert!(!chapter.contains("https://"));
        }
        assert_eq!(fs::read_to_string(source).unwrap(), input);
    }

    #[test]
    fn fb2_roundtrip_retains_reading_order_and_embedded_images_with_valid_metadata() {
        let directory = TempDir::new().unwrap();
        let epub = directory.path().join("source.epub");
        build_epub(&book(), &metadata())
            .unwrap()
            .write(&epub, 9)
            .unwrap();
        let converter = Converter::new(PathBuf::from("/missing-engine"));
        let fb2 = directory.path().join("result.fb2");
        let report = converter
            .convert(&epub, &fb2, BookFormat::Fb2, &metadata())
            .unwrap();
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("integer series"))
        );
        let source = fs::read_to_string(&fb2).unwrap();
        let parsed = xml(&source).unwrap();
        assert!(
            parsed
                .descendants()
                .any(|node| node.has_tag_name((FB2, "genre")) && node.text() == Some("sf_fantasy"))
        );
        let sequence = parsed
            .descendants()
            .find(|node| node.has_tag_name((FB2, "sequence")))
            .unwrap();
        assert_eq!(sequence.attribute("number"), None);
        assert!(parsed.descendants().any(|node| {
            node.has_tag_name((FB2, "document-info"))
                && node
                    .children()
                    .any(|child| child.has_tag_name((FB2, "date")))
        }));
        let reconstructed = directory.path().join("back.epub");
        converter
            .convert(&fb2, &reconstructed, BookFormat::Epub, &metadata())
            .unwrap();
        let document = EpubDocument::open(&reconstructed).unwrap();
        assert_eq!(document.spine.len(), 2);
        assert_text_order(&document.spine_texts().unwrap().join(" "));
        assert_eq!(
            document
                .manifest
                .iter()
                .filter(|item| item.media_type == "image/png")
                .count(),
            1
        );
        assert_eq!(document.metadata.series_index, Some(3.5));
    }

    #[test]
    fn native_mobi_unicode_records_are_bounded_and_validated_by_libmobi() {
        let directory = TempDir::new().unwrap();
        let converter = Converter::new(engine());
        assert!(
            converter.engine_available(),
            "build the bundled libmobi sidecar before running integration tests"
        );
        let mut native = book();
        native.sections[1].blocks.push(Block::Text {
            text: "Long texte français Ελληνικά 日本語. ".repeat(500),
            kind: TextKind::Paragraph,
        });
        let epub = directory.path().join("source.epub");
        build_epub(&native, &metadata())
            .unwrap()
            .write(&epub, 9)
            .unwrap();
        let original = fs::read(&epub).unwrap();
        let mobi = directory.path().join("result.mobi");
        let report = converter
            .convert(&epub, &mobi, BookFormat::Mobi, &metadata())
            .unwrap();
        assert_eq!(report.images_preserved, 1);
        let bytes = fs::read(&mobi).unwrap();
        validate_native_mobi(&bytes).unwrap();
        let destination = directory.path().join("back.epub");
        converter
            .convert(&mobi, &destination, BookFormat::Epub, &metadata())
            .unwrap();
        let reconstructed = EpubDocument::open(&destination).unwrap();
        let text = reconstructed.spine_texts().unwrap().join(" ");
        assert_text_order(&text);
        assert_eq!(
            text.matches("Long texte français Ελληνικά 日本語.").count(),
            500
        );
        assert_eq!(reconstructed.metadata.title, metadata().title);
        assert_eq!(reconstructed.metadata.authors, metadata().authors);
        assert!(
            reconstructed
                .manifest
                .iter()
                .any(|item| item.media_type == "image/png")
        );
        assert_eq!(fs::read(&epub).unwrap(), original);
    }

    #[test]
    fn public_libmobi_hybrid_sample_is_reconstructed_without_source_mutation() {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/libmobi-0.12/tests/samples/sample-unicode-uncompressed.mobi");
        let before = fs::read(&source).unwrap();
        let directory = TempDir::new().unwrap();
        let destination = directory.path().join("sample.epub");
        Converter::new(engine())
            .convert(&source, &destination, BookFormat::Epub, &metadata())
            .unwrap();
        let document = EpubDocument::open(&destination).unwrap();
        assert!(!document.spine.is_empty());
        assert_eq!(document.spine.len(), 2);
        assert!(
            document
                .spine_texts()
                .unwrap()
                .join(" ")
                .contains("Libmobi features")
        );
        assert_eq!(fs::read(source).unwrap(), before);
    }

    #[test]
    fn formats_limits_metadata_and_xml_declarations_are_enforced() {
        let converter = Converter::new(PathBuf::from("/missing-engine"));
        let capabilities = converter.capabilities();
        assert!(
            capabilities
                .iter()
                .find(|capability| capability.inputs.contains(&BookFormat::Pdf))
                .unwrap()
                .outputs
                .is_empty()
        );
        assert!(
            capabilities
                .iter()
                .find(|capability| capability.inputs.contains(&BookFormat::Mobi))
                .unwrap()
                .outputs
                .is_empty()
        );
        let directory = TempDir::new().unwrap();
        let source = directory.path().join("input.txt");
        fs::write(&source, "Readable text").unwrap();
        let target = directory.path().join("result.epub");
        let mut invalid_metadata = metadata();
        invalid_metadata.series_index = Some(f64::NAN);
        assert!(
            converter
                .convert(&source, &target, BookFormat::Epub, &invalid_metadata)
                .is_err()
        );
        assert!(!target.exists());
        OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap()
            .set_len(MAX_TEXT_BYTES + 1)
            .unwrap();
        assert!(
            converter
                .convert(&source, &target, BookFormat::Epub, &metadata())
                .is_err()
        );
        assert!(!target.exists());
        assert!(book_from_fb2("<!DOCTYPE FictionBook [<!ENTITY x 'injected'>]><FictionBook xmlns='http://www.gribuser.ru/xml/fictionbook/2.0'><body><section><p>&x;</p></section></body></FictionBook>", &mut Vec::new()).is_err());
        assert!(xml("<p><![CDATA[<!ENTITY example>]]></p>").is_ok());
    }

    #[test]
    fn mobi_native_output_works_without_an_installed_input_engine() {
        let directory = TempDir::new().unwrap();
        let source = directory.path().join("source.txt");
        fs::write(&source, "Unicode éè— 日本語\n\nDernier paragraphe.").unwrap();
        let destination = directory.path().join("result.mobi");
        let converter = Converter::new(PathBuf::from("/missing-engine"));
        converter
            .convert(&source, &destination, BookFormat::Mobi, &metadata())
            .unwrap();
        let bytes = fs::read(&destination).unwrap();
        validate_native_mobi(&bytes).unwrap();
        assert!(
            bytes
                .windows("Unicode éè— 日本語".len())
                .any(|window| window == "Unicode éè— 日本語".as_bytes())
        );
    }

    #[test]
    fn drm_input_is_refused_without_publishing_a_partial_destination() {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/libmobi-0.12/tests/samples/sample-drm-v1.mobi");
        let directory = TempDir::new().unwrap();
        let destination = directory.path().join("result.epub");
        assert!(
            Converter::new(engine())
                .convert(&source, &destination, BookFormat::Epub, &metadata())
                .is_err()
        );
        assert!(!destination.exists());
    }

    #[test]
    fn sidecar_output_is_bounded_and_credentials_are_not_inherited() {
        let directory = TempDir::new().unwrap();
        let executable = directory.path().join("test-engine");
        fs::write(&executable, "#!/bin/sh\n[ -z \"$HOME\" ] || exit 19\n[ -z \"$SSH_AUTH_SOCK\" ] || exit 20\ni=0\nwhile [ $i -lt 10000 ]; do printf 'a bounded diagnostic line of output\\n'; i=$((i+1)); done\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let source = directory.path().join("input.mobi");
        fs::write(&source, "not a book").unwrap();
        let error = run_mobitool(&executable, &source, directory.path()).unwrap_err();
        assert!(
            matches!(error, AppError::InvalidInput(_)),
            "diagnostic size must be the rejecting condition, after the cleared environment checks"
        );
    }
}
