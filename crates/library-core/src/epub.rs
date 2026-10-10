//! Bounded EPUB inspection and metadata rewriting without extracting ZIP paths.

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{BufReader, Cursor, Read, Seek, Write},
    path::Path,
};

use chrono::Utc;
use roxmltree::{Document, Node, ParsingOptions};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{AppError, BookMetadata, Result};

const DC: &str = "http://purl.org/dc/elements/1.1/";
const OPF: &str = "http://www.idpf.org/2007/opf";
const EPUB_MIMETYPE: &[u8] = b"application/epub+zip";
const MAX_ENTRIES: usize = 20_000;
const MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 1_000;
const MAX_XML_NODES: u32 = 500_000;
const MAX_INSPECTION_SAMPLE_CHARACTERS: usize = 12_000;
const MAX_INVENTORY_XML_BYTES: u64 = 2 * 1024 * 1024;
const MAX_EDITION_ENTRY_BYTES: usize = 512 * 1024;
const MAX_EDITION_REFERENCES: usize = 32;
const EDITION_CANDIDATE_LIMITS: [usize; 3] = [2, 2, 4];
const EPUB_OPS: &str = "http://www.idpf.org/2007/ops";

/// Catalogue metadata only: resource validation remains the responsibility of import.
/// Reading three bounded entries avoids transferring chapters and images over slow USB.
pub fn read_inventory_metadata(path: &Path) -> Result<BookMetadata> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(invalid("EPUB source must be a regular file"));
    }
    let fallback = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Untitled");
    read_inventory_metadata_file(File::open(path)?, fallback)
}

/// Reads only bounded catalogue metadata from an already-opened source.
/// Callers retain responsibility for no-follow opening and post-read identity
/// checks; this function never reopens the source by its original pathname.
pub fn read_inventory_metadata_file(file: File, fallback: &str) -> Result<BookMetadata> {
    if !file.metadata()?.file_type().is_file() {
        return Err(invalid("EPUB source must be a regular file"));
    }
    let mut archive = ZipArchive::new(BufReader::new(file))?;
    if archive.len() > MAX_ENTRIES {
        return Err(invalid("EPUB archive has too many entries"));
    }
    let mut names = HashSet::new();
    for name in archive.file_names() {
        validate_archive_name(name)?;
        if !names.insert(name.trim_end_matches('/').to_lowercase()) {
            return Err(invalid("ZIP contains duplicate or case-colliding paths"));
        }
    }
    if archive.index_for_name("mimetype") != Some(0) {
        return Err(invalid("EPUB mimetype must be first and uncompressed"));
    }
    {
        let entry = archive.by_name("mimetype")?;
        if entry.compression() != CompressionMethod::Stored || entry.header_start() != 0 {
            return Err(invalid("EPUB mimetype must be first and uncompressed"));
        }
    }
    if read_inventory_entry(&mut archive, "mimetype", EPUB_MIMETYPE.len() as u64)? != EPUB_MIMETYPE
    {
        return Err(invalid("Invalid EPUB mimetype"));
    }
    let container = read_inventory_entry(
        &mut archive,
        "META-INF/container.xml",
        MAX_INVENTORY_XML_BYTES,
    )?;
    let container = std::str::from_utf8(&container)
        .map_err(|_| AppError::Unsupported("EPUB XML must be UTF-8".into()))?;
    let opf_path = package_path_from_container(container)?;
    let opf = read_inventory_entry(&mut archive, &opf_path, MAX_INVENTORY_XML_BYTES)?;
    let opf = std::str::from_utf8(&opf)
        .map_err(|_| AppError::Unsupported("EPUB XML must be UTF-8".into()))?;
    let document = parse_xml(opf)?;
    let package = document.root_element();
    validate_package(package)?;
    let metadata = package
        .children()
        .find(|node| node.has_tag_name((OPF, "metadata")))
        .ok_or_else(|| invalid("OPF metadata is missing"))?;
    Ok(read_metadata(metadata, fallback))
}

fn read_inventory_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>> {
    let mut entry = archive.by_name(name)?;
    if entry.encrypted() {
        return Err(AppError::Unsupported(
            "Encrypted ZIP entries are unsupported".into(),
        ));
    }
    if entry.is_dir() || !matches!(entry.unix_mode().unwrap_or(0) & 0o170000, 0 | 0o100000) {
        return Err(invalid("ZIP metadata must be a regular file"));
    }
    let declared = entry.size();
    if declared > limit {
        return Err(invalid("EPUB inventory metadata exceeds the safety limit"));
    }
    if declared
        > entry
            .compressed_size()
            .saturating_mul(MAX_COMPRESSION_RATIO)
    {
        return Err(invalid("ZIP compression ratio exceeds the safety limit"));
    }
    let mut bytes = Vec::new();
    entry.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != declared {
        return Err(invalid("ZIP entry has an inconsistent uncompressed size"));
    }
    Ok(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestItem {
    pub id: String,
    /// Canonical path inside the archive, already resolved from the OPF path.
    pub href: String,
    pub media_type: String,
    pub properties: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub label: String,
    /// Canonical archive path, followed by an optional fragment identifier.
    pub href: String,
    pub children: Vec<Self>,
}

#[derive(Debug, Clone)]
pub struct EpubDocument {
    pub entries: BTreeMap<String, Vec<u8>>,
    pub metadata: BookMetadata,
    pub opf_path: String,
    pub manifest: Vec<ManifestItem>,
    pub spine: Vec<String>,
    pub toc: Vec<TocEntry>,
    pub cover_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EpubInspection {
    pub metadata: BookMetadata,
    pub cover_path: Option<String>,
    pub chapter_count: u64,
    pub text_sample: String,
    pub warnings: Vec<String>,
    /// Structural and XML validation; profile-specific image/CSS checks still apply.
    pub transformable: bool,
}

pub fn inspect_for_import(path: &Path) -> Result<EpubInspection> {
    let (entries, fallback) = read_archive(path)?;
    validate_entries(&entries)?;
    let opf_path = find_package(&entries)?;
    let document = parse_xml(entry_text(&entries, &opf_path)?)?;
    let package = document.root_element();
    if !package.has_tag_name((OPF, "package")) {
        return Err(invalid("OPF root is not a package"));
    }
    let mut warnings = Vec::new();
    let mut transformable = true;
    if validate_package(package).is_err() {
        transformable = false;
        warnings
            .push("OPF contains duplicate or empty XML IDs; original retained for review".into());
    }
    let metadata_node = child_named(package, "metadata");
    let metadata = if let Some(node) = metadata_node {
        read_metadata(node, &fallback)
    } else {
        transformable = false;
        warnings.push("OPF metadata is missing; filename used as title".into());
        BookMetadata {
            title: fallback,
            language: "und".into(),
            ..Default::default()
        }
    };
    let mut manifest = Vec::new();
    let mut id_counts = HashMap::<String, usize>::new();
    if let Some(node) = child_named(package, "manifest") {
        for item in node
            .children()
            .filter(|node| node.has_tag_name((OPF, "item")))
        {
            let id = item.attribute("id").unwrap_or_default();
            *id_counts.entry(id.into()).or_default() += 1;
            let resolved = item
                .attribute("href")
                .and_then(|href| resolve_href(&opf_path, href).ok());
            if id.is_empty()
                || resolved
                    .as_ref()
                    .is_none_or(|href| !entries.contains_key(href))
            {
                transformable = false;
                warnings.push("Manifest contains an invalid or missing resource; original retained for review".into());
                continue;
            }
            manifest.push(ManifestItem {
                id: id.into(),
                href: resolved.expect("checked resource"),
                media_type: item.attribute("media-type").unwrap_or_default().into(),
                properties: item.attribute("properties").unwrap_or_default().into(),
            });
        }
    } else {
        transformable = false;
        warnings.push("OPF manifest is missing".into());
    }
    if id_counts.values().any(|count| *count > 1) {
        transformable = false;
        warnings.push("Duplicate manifest IDs make some reading-order references ambiguous".into());
    }
    manifest.retain(|item| id_counts.get(&item.id) == Some(&1));
    let cover_path = metadata_node.and_then(|metadata| find_cover(metadata, &manifest));
    let spine_node = child_named(package, "spine");
    let references: Vec<_> = spine_node
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|node| node.has_tag_name((OPF, "itemref")))
        .collect();
    let mut sample = String::new();
    let mut sample_characters = 0;
    if references.is_empty() {
        transformable = false;
        warnings.push("Reading order is missing or empty".into());
    }
    for reference in &references {
        let item = reference
            .attribute("idref")
            .and_then(|id| manifest.iter().find(|item| item.id == id));
        let Some(item) = item else {
            transformable = false;
            warnings.push("A reading-order resource is missing or ambiguous".into());
            continue;
        };
        if !matches!(
            item.media_type.as_str(),
            "application/xhtml+xml" | "text/html" | "image/svg+xml"
        ) {
            transformable = false;
            warnings.push("A reading-order resource has an unsupported media type".into());
            continue;
        }
        let text = match text_for_inspection(entry_text(&entries, &item.href)?) {
            Ok((text, relaxed)) => {
                if relaxed {
                    transformable = false;
                    warnings.push("HTML-compatible text inspection used; source XML requires review before transformation".into());
                }
                text
            }
            Err(_) => {
                transformable = false;
                warnings.push("A chapter could not be inspected safely".into());
                continue;
            }
        };
        if sample_characters < MAX_INSPECTION_SAMPLE_CHARACTERS {
            if !sample.is_empty() {
                sample.push('\n');
                sample_characters += 1;
            }
            let remaining = MAX_INSPECTION_SAMPLE_CHARACTERS - sample_characters;
            let chunk: String = text.chars().take(remaining).collect();
            sample_characters += chunk.chars().count();
            sample.push_str(&chunk);
        }
    }
    warnings.sort();
    warnings.dedup();
    Ok(EpubInspection {
        metadata,
        cover_path,
        chapter_count: references.len() as u64,
        text_sample: sample,
        warnings,
        transformable,
    })
}

/// Plain text for catalogue/reader fallback, never HTML to render directly.
pub fn text_for_inspection(source: &str) -> Result<(String, bool)> {
    validate_content_doctype(source)?;
    if let Ok(text) = strict_content_text(source) {
        return Ok((text, false));
    }
    let sanitized = ammonia::Builder::default()
        .add_clean_content_tags(&["title"])
        .generic_attributes(HashSet::new())
        .tag_attributes(HashMap::new())
        .link_rel(None)
        .clean(source)
        .to_string();
    let mut text = String::with_capacity(sanitized.len());
    let mut inside_tag = false;
    for character in sanitized.chars() {
        if character == '<' {
            inside_tag = true;
            text.push(' ');
        } else if character == '>' && inside_tag {
            inside_tag = false;
            text.push(' ');
        } else if !inside_tag {
            text.push(character);
        }
    }
    let wrapped = format!("<body>{}</body>", normalized_standard_entities(&text));
    let parsed = parse_document(&wrapped, false)?;
    Ok((node_text(parsed.root_element()), true))
}

fn strict_content_text(source: &str) -> Result<String> {
    validate_content_doctype(source)?;
    let normalized = normalized_standard_entities(source);
    let document = parse_markup(&normalized)?;
    let root = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "body")
        .unwrap_or(document.root_element());
    Ok(node_text(root))
}

fn ignored_markup_ending(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"<![CDATA[") {
        Some("]]>")
    } else if bytes.starts_with(b"<!--") {
        Some("-->")
    } else if bytes.starts_with(b"<?") {
        Some("?>")
    } else {
        None
    }
}

fn normalized_standard_entities(source: &str) -> Cow<'_, str> {
    if !source.contains("&nbsp;") {
        return Cow::Borrowed(source);
    }
    let bytes = source.as_bytes();
    let mut normalized = source.to_owned();
    let mut index = 0;
    while index < bytes.len() {
        let ending = ignored_markup_ending(&bytes[index..]);
        if let Some(ending) = ending {
            if let Some(offset) = source[index..].find(ending) {
                index += offset + ending.len();
            } else {
                break;
            }
        } else if bytes[index..].starts_with(b"<!DOCTYPE") {
            let mut quote = None;
            while let Some(&byte) = bytes.get(index) {
                index += 1;
                if let Some(delimiter) = quote {
                    if byte == delimiter {
                        quote = None;
                    }
                } else if matches!(byte, b'\'' | b'"') {
                    quote = Some(byte);
                } else if byte == b'>' {
                    break;
                }
            }
        } else if bytes[index..].starts_with(b"&nbsp;") {
            normalized.replace_range(index..index + 6, "&#160;");
            index += 6;
        } else {
            index += 1;
        }
    }
    Cow::Owned(normalized)
}

fn read_archive(path: &Path) -> Result<(BTreeMap<String, Vec<u8>>, String)> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(invalid("EPUB source must be a regular file"));
    }
    let fallback = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Untitled")
        .to_owned();
    let reader = File::open(path)?;
    read_archive_reader(reader, fallback)
}

fn read_archive_reader<R: Read + Seek>(
    reader: R,
    fallback: String,
) -> Result<(BTreeMap<String, Vec<u8>>, String)> {
    let mut archive = ZipArchive::new(reader)?;
    if archive.len() > MAX_ENTRIES {
        return Err(invalid("EPUB archive has too many entries"));
    }
    let mut entries = BTreeMap::new();
    let mut names = HashSet::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| invalid("ZIP names must be UTF-8"))?
            .to_owned();
        validate_archive_name(&name)?;
        if !names.insert(name.trim_end_matches('/').to_lowercase()) {
            return Err(invalid("ZIP contains duplicate or case-colliding paths"));
        }
        if entry.encrypted() {
            return Err(AppError::Unsupported(
                "Encrypted ZIP entries are unsupported".into(),
            ));
        }
        let file_type = entry.unix_mode().unwrap_or(0) & 0o170000;
        if !matches!(file_type, 0 | 0o100000 | 0o040000) {
            return Err(invalid("ZIP links and special files are forbidden"));
        }
        if index == 0
            && (name != "mimetype"
                || entry.compression() != CompressionMethod::Stored
                || entry.header_start() != 0)
        {
            return Err(invalid("EPUB mimetype must be first and uncompressed"));
        }
        if entry.is_dir() {
            continue;
        }
        let declared = entry.size();
        total = total
            .checked_add(declared)
            .ok_or_else(|| invalid("ZIP size overflow"))?;
        if declared > MAX_ENTRY_BYTES || total > MAX_TOTAL_BYTES {
            return Err(invalid("EPUB uncompressed size exceeds the safety limit"));
        }
        if declared
            > entry
                .compressed_size()
                .saturating_mul(MAX_COMPRESSION_RATIO)
        {
            return Err(invalid("ZIP compression ratio exceeds the safety limit"));
        }
        let mut bytes = Vec::new();
        entry
            .by_ref()
            .take(MAX_ENTRY_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != declared {
            return Err(invalid("ZIP entry has an inconsistent uncompressed size"));
        }
        entries.insert(name, bytes);
    }
    Ok((entries, fallback))
}

impl EpubDocument {
    /// Decode bytes obtained through managed storage without reopening a path.
    pub fn from_bytes(bytes: &[u8], fallback_title: &str) -> Result<Self> {
        if bytes.len() as u64 > MAX_TOTAL_BYTES || fallback_title.len() > 4096 {
            return Err(invalid(
                "EPUB archive or fallback title exceeds the safety limit",
            ));
        }
        let (entries, fallback) =
            read_archive_reader(Cursor::new(bytes), fallback_title.to_owned())?;
        Self::from_entries(entries, &fallback)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let (entries, fallback) = read_archive(path)?;
        Self::from_entries(entries, &fallback)
    }

    /// Canonicalize a known XHTML entity in a derived document, preserving literal sections.
    pub fn normalize_standard_entities(&mut self) -> Result<()> {
        for item in &self.manifest {
            if matches!(
                item.media_type.as_str(),
                "application/xhtml+xml"
                    | "text/html"
                    | "image/svg+xml"
                    | "application/x-dtbncx+xml"
            ) {
                let source = entry_text(&self.entries, &item.href)?;
                validate_content_doctype(source)?;
                let normalized = normalized_standard_entities(source);
                if normalized != source {
                    self.entries
                        .insert(item.href.clone(), normalized.into_owned().into_bytes());
                }
            }
        }
        Ok(())
    }

    /// Construct a validated document for native format converters.
    pub fn from_entries(entries: BTreeMap<String, Vec<u8>>, fallback_title: &str) -> Result<Self> {
        validate_entries(&entries)?;
        let opf_path = find_package(&entries)?;
        let opf_text = entry_text(&entries, &opf_path)?;
        let document = parse_xml(opf_text)?;
        let package = document.root_element();
        validate_package(package)?;
        let metadata_node =
            child_named(package, "metadata").ok_or_else(|| invalid("OPF metadata is missing"))?;
        let metadata = read_metadata(metadata_node, fallback_title);
        let manifest = read_manifest(package, &opf_path, &entries)?;
        let spine = read_spine(package, &manifest)?;
        let cover_path = find_cover(metadata_node, &manifest);
        let toc = read_toc(&entries, &manifest, &spine);
        Ok(Self {
            entries,
            metadata,
            opf_path,
            manifest,
            spine,
            toc,
            cover_path,
        })
    }

    /// Sample declared edition pages before narrative content, without changing the archive.
    /// This operates on an already validated document; archive loading retains its own bounds.
    pub fn edition_sample(&self, max_chars: usize) -> Result<String> {
        let limit = max_chars.min(MAX_INSPECTION_SAMPLE_CHARACTERS);
        if limit == 0 {
            return Ok(String::new());
        }
        let text_paths: HashSet<&str> = self
            .manifest
            .iter()
            .filter(|item| {
                matches!(
                    item.media_type.as_str(),
                    "application/xhtml+xml" | "text/html"
                )
            })
            .map(|item| item.href.as_str())
            .collect();
        let navigation_paths: HashSet<&str> = self
            .manifest
            .iter()
            .filter(|item| {
                item.properties
                    .split_whitespace()
                    .any(|property| property == "nav")
            })
            .map(|item| item.href.as_str())
            .collect();
        let mut candidates: [Vec<String>; 3] = Default::default();
        let opf = entry_text(&self.entries, &self.opf_path)?;
        if opf.len() as u64 > MAX_INVENTORY_XML_BYTES {
            return Err(invalid("Edition sample metadata exceeds the safety limit"));
        }
        let package = parse_xml(opf)?;
        if let Some(guide) = child_named(package.root_element(), "guide") {
            for reference in guide
                .children()
                .filter(|node| node.is_element() && node.tag_name().name() == "reference")
                .take(MAX_EDITION_REFERENCES)
            {
                if let Some(role) = reference.attribute("type").and_then(edition_page_role) {
                    let href = reference
                        .attribute("href")
                        .ok_or_else(|| invalid("Edition reference has no path"))?;
                    let path = resolve_href(&self.opf_path, href)?;
                    if !text_paths.contains(path.as_str()) {
                        return Err(invalid("Edition reference is not a manifest text resource"));
                    }
                    add_edition_candidate(&mut candidates, role, &path);
                }
            }
        }
        if let Some(nav_item) = self.manifest.iter().find(|item| {
            item.properties
                .split_whitespace()
                .any(|property| property == "nav")
        }) {
            let bytes = self
                .entries
                .get(&nav_item.href)
                .ok_or_else(|| invalid("Edition navigation is missing"))?;
            if bytes.len() <= MAX_EDITION_ENTRY_BYTES {
                let source = std::str::from_utf8(bytes)
                    .map_err(|_| invalid("Edition navigation is not UTF8"))?;
                validate_content_doctype(source)?;
                // Malformed optional HTML navigation cannot manufacture a trusted reference.
                if let Ok(nav) = parse_markup(source) {
                    for landmark in nav
                        .descendants()
                        .filter(|node| {
                            node.is_element()
                                && node.tag_name().name() == "nav"
                                && node.attribute((EPUB_OPS, "type")).is_some_and(|types| {
                                    types.split_whitespace().any(|kind| kind == "landmarks")
                                })
                        })
                        .take(1)
                    {
                        for link in landmark
                            .descendants()
                            .filter(|node| node.is_element() && node.tag_name().name() == "a")
                            .take(MAX_EDITION_REFERENCES)
                        {
                            if let Some(role) = link
                                .attribute((EPUB_OPS, "type"))
                                .and_then(edition_page_role)
                            {
                                let href = link
                                    .attribute("href")
                                    .ok_or_else(|| invalid("Edition landmark has no path"))?;
                                let path = resolve_href(&nav_item.href, href)?;
                                if !text_paths.contains(path.as_str()) {
                                    return Err(invalid(
                                        "Edition landmark is not a manifest text resource",
                                    ));
                                }
                                add_edition_candidate(&mut candidates, role, &path);
                            }
                        }
                    }
                }
            }
        }
        // Filenames rank candidates only; their contents never become inferred metadata.
        for item in &self.manifest {
            if text_paths.contains(item.href.as_str()) {
                let name = item
                    .href
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let role = if ["titlepage", "title-page", "title_page"]
                    .iter()
                    .any(|hint| name.contains(hint))
                {
                    Some(0)
                } else if ["copyright", "colophon", "imprint", "legal", "rights"]
                    .iter()
                    .any(|hint| name.contains(hint))
                {
                    Some(1)
                } else {
                    None
                };
                if let Some(role) = role {
                    add_edition_candidate(&mut candidates, role, &item.href);
                }
            }
        }
        for path in &self.spine {
            if text_paths.contains(path.as_str()) && !navigation_paths.contains(path.as_str()) {
                add_edition_candidate(&mut candidates, 2, path);
                if candidates[2].len() == EDITION_CANDIDATE_LIMITS[2] {
                    break;
                }
            }
        }
        let mut sections: [String; 3] = Default::default();
        for (role, paths) in candidates.iter().enumerate() {
            for path in paths {
                let bytes = self
                    .entries
                    .get(path)
                    .ok_or_else(|| invalid("Edition sample page is missing"))?;
                if bytes.len() > MAX_EDITION_ENTRY_BYTES {
                    continue;
                }
                let source = std::str::from_utf8(bytes)
                    .map_err(|_| invalid("Edition sample page is not UTF8"))?;
                let (text, _) = text_for_inspection(source)?;
                let text: String = text
                    .nfc()
                    .filter(|character| !character.is_control() || character.is_whitespace())
                    .collect();
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !text.is_empty() {
                    sections[role] = text;
                    break;
                }
            }
        }
        if sections.iter().all(String::is_empty) {
            return Err(AppError::Unsupported(
                "No bounded EPUB edition sample is available".into(),
            ));
        }
        let mut quotas = [limit / 4, limit / 4, limit - 2 * (limit / 4)];
        if limit < 4 {
            quotas = [0; 3];
            for (role, _) in sections
                .iter()
                .enumerate()
                .filter(|(_, text)| !text.is_empty())
                .take(limit)
            {
                quotas[role] = 1;
            }
        }
        let mut available = 0;
        for (section, quota) in sections.iter().zip(quotas.iter_mut()) {
            if section.is_empty() {
                available += *quota;
                *quota = 0;
            }
        }
        let present = sections
            .iter()
            .filter(|section| !section.is_empty())
            .count();
        let extra = available / present;
        let mut remainder = available % present;
        for (section, quota) in sections.iter().zip(quotas.iter_mut()) {
            if !section.is_empty() {
                *quota += extra;
                if remainder > 0 {
                    *quota += 1;
                    remainder -= 1;
                }
            }
        }
        let mut output = String::new();
        for (section, quota) in sections.iter().zip(quotas) {
            if quota == 0 || section.is_empty() {
                continue;
            }
            let separator = usize::from(!output.is_empty());
            if quota <= separator {
                continue;
            }
            if separator != 0 {
                output.push('\n');
            }
            output.extend(section.chars().take(quota - separator));
        }
        Ok(output)
    }

    /// Writes a new file only. Staging, replacement and backup belong to storage.
    pub fn write(&self, path: &Path, level: u8) -> Result<()> {
        if level > 9 {
            return Err(invalid(
                "ZIP compression level must be between zero and nine",
            ));
        }
        validate_entries(&self.entries)?;
        let opf_path = find_package(&self.entries)?;
        let document = parse_xml(entry_text(&self.entries, &opf_path)?)?;
        validate_package(document.root_element())?;
        let manifest = read_manifest(document.root_element(), &opf_path, &self.entries)?;
        read_spine(document.root_element(), &manifest)?;
        let output = OpenOptions::new().write(true).create_new(true).open(path)?;
        let result = write_zip(output, &self.entries, level);
        if result.is_err() {
            let _ = fs::remove_file(path);
        }
        result
    }

    pub fn update_metadata(&mut self, metadata: &BookMetadata) -> Result<()> {
        validate_metadata(metadata)?;
        let source = entry_text(&self.entries, &self.opf_path)?;
        let document = parse_xml(source)?;
        let package = document.root_element();
        let node =
            child_named(package, "metadata").ok_or_else(|| invalid("OPF metadata is missing"))?;
        let replacement = rewrite_metadata(source, package, node, metadata)?;
        let mut rewritten = String::with_capacity(source.len() + replacement.len());
        rewritten.push_str(&source[..node.range().start]);
        rewritten.push_str(&replacement);
        rewritten.push_str(&source[node.range().end..]);
        parse_xml(&rewritten)?;
        self.entries
            .insert(self.opf_path.clone(), rewritten.into_bytes());
        self.metadata = metadata.clone();
        Ok(())
    }

    pub fn spine_texts(&self) -> Result<Vec<String>> {
        self.spine
            .iter()
            .map(|path| strict_content_text(entry_text(&self.entries, path)?))
            .collect()
    }

    /// Chapter boundaries are framed to detect missing, reordered or merged text.
    pub fn text_fingerprint(&self) -> Result<String> {
        let mut hash = Sha256::new();
        for text in self.spine_texts()? {
            hash.update((text.len() as u64).to_le_bytes());
            hash.update(text.as_bytes());
        }
        Ok(hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
}

fn invalid(message: &str) -> AppError {
    AppError::InvalidInput(message.into())
}

fn validate_package(package: Node<'_, '_>) -> Result<()> {
    if !package.has_tag_name((OPF, "package")) {
        return Err(invalid("OPF root is not a package"));
    }
    let mut ids = HashSet::new();
    for id in package
        .descendants()
        .filter_map(|node| node.attribute("id"))
    {
        if id.is_empty() || !ids.insert(id) {
            return Err(invalid("OPF contains duplicate or empty XML IDs"));
        }
    }
    Ok(())
}

fn validate_archive_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains(['\\', '\0'])
        || name
            .split('/')
            .any(|part| part == ".." || part == "." || part.contains(':'))
        || name.trim_end_matches('/').split('/').any(str::is_empty)
    {
        return Err(invalid("Unsafe ZIP path"));
    }
    Ok(())
}

fn edition_page_role(types: &str) -> Option<usize> {
    types.split_whitespace().find_map(|kind| match kind {
        "title-page" | "titlepage" => Some(0),
        "copyright-page" | "copyright" | "colophon" => Some(1),
        _ => None,
    })
}

fn add_edition_candidate(candidates: &mut [Vec<String>; 3], role: usize, path: &str) {
    if candidates[role].len() < EDITION_CANDIDATE_LIMITS[role]
        && !candidates
            .iter()
            .any(|paths| paths.iter().any(|candidate| candidate == path))
    {
        candidates[role].push(path.into());
    }
}

fn validate_entries(entries: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    if entries.len() > MAX_ENTRIES
        || entries.get("mimetype").map(Vec::as_slice) != Some(EPUB_MIMETYPE)
    {
        return Err(invalid("Invalid EPUB mimetype or entry count"));
    }
    let mut names = HashSet::new();
    let mut total = 0_u64;
    for (name, bytes) in entries {
        validate_archive_name(name)?;
        if !names.insert(name.to_lowercase()) {
            return Err(invalid("EPUB contains case-colliding paths"));
        }
        total += bytes.len() as u64;
        if bytes.len() as u64 > MAX_ENTRY_BYTES || total > MAX_TOTAL_BYTES {
            return Err(invalid("EPUB content exceeds the safety limit"));
        }
    }
    if entries.contains_key("META-INF/signatures.xml") {
        return Err(AppError::Unsupported(
            "Signed EPUBs cannot be transformed safely".into(),
        ));
    }
    check_encryption(entries)
}

fn parse_xml(source: &str) -> Result<Document<'_>> {
    if source.contains("<!DOCTYPE") || source.contains("<!ENTITY") {
        return Err(invalid(
            "DTDs and XML entities are forbidden in EPUB metadata",
        ));
    }
    parse_document(source, false)
}

pub(crate) fn parse_markup(source: &str) -> Result<Document<'_>> {
    validate_content_doctype(source)?;
    parse_document(source, true)
}

fn validate_content_doctype(source: &str) -> Result<()> {
    let bytes = source.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        if let Some(ending) = ignored_markup_ending(&bytes[offset..]) {
            offset = source[offset..]
                .find(ending)
                .map(|position| offset + position + ending.len())
                .unwrap_or(bytes.len());
            continue;
        }
        if bytes[offset..].starts_with(b"<!ENTITY") {
            return Err(invalid(
                "XML entity declarations are forbidden in EPUB content",
            ));
        }
        if !bytes[offset..].starts_with(b"<!DOCTYPE") {
            offset += 1;
            continue;
        }
        let mut position = offset + "<!DOCTYPE".len();
        let mut quote = None;
        let mut complete = false;
        while let Some(&byte) = bytes.get(position) {
            if let Some(delimiter) = quote {
                if byte == delimiter {
                    quote = None;
                }
            } else if matches!(byte, b'\'' | b'"') {
                quote = Some(byte);
            } else if matches!(byte, b'[' | b']') {
                return Err(invalid(
                    "Internal DTD subsets are forbidden in EPUB content",
                ));
            } else if byte == b'>' {
                offset = position + 1;
                complete = true;
                break;
            }
            position += 1;
        }
        if !complete {
            return Err(invalid(
                "EPUB content has an incomplete DOCTYPE declaration",
            ));
        }
    }
    Ok(())
}

fn parse_document(source: &str, allow_dtd: bool) -> Result<Document<'_>> {
    Document::parse_with_options(
        source,
        ParsingOptions {
            allow_dtd,
            nodes_limit: MAX_XML_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|_| invalid("EPUB contains invalid or unsupported XML"))
}

fn entry_text<'a>(entries: &'a BTreeMap<String, Vec<u8>>, path: &str) -> Result<&'a str> {
    let bytes = entries
        .get(path)
        .ok_or_else(|| invalid("EPUB references a missing entry"))?;
    std::str::from_utf8(bytes).map_err(|_| AppError::Unsupported("EPUB XML must be UTF-8".into()))
}

fn child_named<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == name)
}

fn check_encryption(entries: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let Some(bytes) = entries.get("META-INF/encryption.xml") else {
        return Ok(());
    };
    let source =
        std::str::from_utf8(bytes).map_err(|_| invalid("Invalid encryption.xml encoding"))?;
    let document = parse_xml(source)?;
    for method in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "EncryptionMethod")
    {
        if !matches!(
            method.attribute("Algorithm"),
            Some("http://www.idpf.org/2008/embedding" | "http://ns.adobe.com/pdf/enc#RC")
        ) {
            return Err(AppError::Unsupported(
                "DRM-encrypted EPUB content is unsupported".into(),
            ));
        }
    }
    for encrypted in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "EncryptedData")
    {
        if !encrypted
            .descendants()
            .any(|node| node.is_element() && node.tag_name().name() == "EncryptionMethod")
        {
            return Err(invalid("Encrypted resource is missing its algorithm"));
        }
        let reference = encrypted
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "CipherReference")
            .and_then(|node| node.attribute("URI"))
            .ok_or_else(|| invalid("Obfuscated font reference is missing"))?;
        let path = resolve_href("", reference)?;
        let extension = path
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !entries.contains_key(&path)
            || !matches!(extension.as_str(), "ttf" | "otf" | "woff" | "woff2")
        {
            return Err(AppError::Unsupported(
                "Only embedded fonts may use font obfuscation".into(),
            ));
        }
    }
    Ok(())
}

fn find_package(entries: &BTreeMap<String, Vec<u8>>) -> Result<String> {
    let path = package_path_from_container(entry_text(entries, "META-INF/container.xml")?)?;
    if !entries.contains_key(&path) {
        return Err(invalid("Package document is missing"));
    }
    Ok(path)
}

fn package_path_from_container(source: &str) -> Result<String> {
    let container = parse_xml(source)?;
    let rootfiles: Vec<_> = container
        .descendants()
        .filter(|node| {
            node.is_element()
                && node.tag_name().name() == "rootfile"
                && node.attribute("media-type") == Some("application/oebps-package+xml")
        })
        .collect();
    if rootfiles.len() != 1 {
        return Err(AppError::Unsupported(
            "EPUB must have exactly one package document".into(),
        ));
    }
    let full_path = rootfiles[0]
        .attribute("full-path")
        .ok_or_else(|| invalid("Missing package path"))?;
    resolve_href("", full_path)
}

/// Resolve URI-escaped resource paths without crossing the archive root.
pub fn resolve_href(base_path: &str, href: &str) -> Result<String> {
    let path = href.split('#').next().unwrap_or_default();
    if path.contains('?') {
        return Err(invalid("EPUB resource queries are unsupported"));
    }
    let decoded = percent_decode(path)?;
    if decoded.starts_with('/') || decoded.contains(['\\', '\0', ':']) {
        return Err(invalid("External or absolute EPUB resource reference"));
    }
    if decoded.is_empty() {
        if base_path.is_empty() {
            return Err(invalid("Empty EPUB resource path"));
        }
        return Ok(base_path.into());
    }
    let mut parts: Vec<_> = base_path
        .rsplit_once('/')
        .map(|(directory, _)| directory.split('/').collect())
        .unwrap_or_default();
    for part in decoded.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(invalid("Resource path escapes the EPUB archive"));
                }
            }
            value => parts.push(value),
        }
    }
    let resolved = parts.join("/");
    validate_archive_name(&resolved)?;
    Ok(resolved)
}

fn percent_decode(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = bytes
                .get(index + 1..index + 3)
                .ok_or_else(|| invalid("Incomplete URI escape"))?;
            let digits = std::str::from_utf8(pair).map_err(|_| invalid("Invalid URI escape"))?;
            output.push(u8::from_str_radix(digits, 16).map_err(|_| invalid("Invalid URI escape"))?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).map_err(|_| invalid("Resource URI is not UTF-8"))
}

fn read_manifest(
    package: Node<'_, '_>,
    opf_path: &str,
    entries: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<ManifestItem>> {
    let node =
        child_named(package, "manifest").ok_or_else(|| invalid("OPF manifest is missing"))?;
    let mut ids = HashSet::new();
    let mut manifest = Vec::new();
    for item in node
        .children()
        .filter(|child| child.is_element() && child.tag_name().name() == "item")
    {
        let id = item
            .attribute("id")
            .ok_or_else(|| invalid("Manifest item has no ID"))?;
        if id.is_empty() || !ids.insert(id.to_owned()) {
            return Err(invalid("Duplicate or empty manifest ID"));
        }
        let href = resolve_href(
            opf_path,
            item.attribute("href")
                .ok_or_else(|| invalid("Manifest item has no href"))?,
        )?;
        if !entries.contains_key(&href) {
            return Err(invalid("Manifest resource is missing"));
        }
        manifest.push(ManifestItem {
            id: id.into(),
            href,
            media_type: item.attribute("media-type").unwrap_or_default().into(),
            properties: item.attribute("properties").unwrap_or_default().into(),
        });
    }
    Ok(manifest)
}

fn read_spine(package: Node<'_, '_>, manifest: &[ManifestItem]) -> Result<Vec<String>> {
    let node = child_named(package, "spine").ok_or_else(|| invalid("OPF spine is missing"))?;
    let mut spine = Vec::new();
    for item in node
        .children()
        .filter(|child| child.is_element() && child.tag_name().name() == "itemref")
    {
        let id = item
            .attribute("idref")
            .ok_or_else(|| invalid("Spine reference has no ID"))?;
        let resource = manifest
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| invalid("Spine item is not in the manifest"))?;
        if !matches!(
            resource.media_type.as_str(),
            "application/xhtml+xml" | "text/html" | "image/svg+xml"
        ) {
            return Err(AppError::Unsupported(
                "Spine resource is not supported text content".into(),
            ));
        }
        spine.push(resource.href.clone());
    }
    if spine.is_empty() {
        return Err(invalid("EPUB has no reading-order content"));
    }
    Ok(spine)
}

fn node_text(node: Node<'_, '_>) -> String {
    let pieces: Vec<_> = node
        .descendants()
        .filter(|child| {
            child.is_text()
                && !child.ancestors().any(|ancestor| {
                    ancestor.is_element()
                        && matches!(ancestor.tag_name().name(), "script" | "style")
                })
        })
        .filter_map(|child| child.text())
        .collect();
    pieces
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn dc_values(metadata: Node<'_, '_>, local: &str) -> Vec<String> {
    metadata
        .children()
        .filter(|node| node.has_tag_name((DC, local)))
        .map(node_text)
        .filter(|value| !value.is_empty())
        .collect()
}

fn creator_role(metadata: Node<'_, '_>, creator: Node<'_, '_>) -> String {
    if let Some(role) = creator.attribute((OPF, "role")) {
        return role.trim().to_lowercase();
    }
    let Some(id) = creator.attribute("id") else {
        return "aut".into();
    };
    metadata
        .children()
        .find(|node| {
            node.is_element()
                && node.tag_name().name() == "meta"
                && node.attribute("property") == Some("role")
                && node
                    .attribute("refines")
                    .map(|value| value.trim_start_matches('#'))
                    == Some(id)
        })
        .map(node_text)
        .unwrap_or_else(|| "aut".into())
        .to_lowercase()
}

fn read_metadata(node: Node<'_, '_>, fallback: &str) -> BookMetadata {
    let authors: Vec<_> = node
        .children()
        .filter(|child| child.has_tag_name((DC, "creator")) && creator_role(node, *child) == "aut")
        .map(node_text)
        .filter(|value| !value.is_empty())
        .collect();
    let first_creator = node
        .children()
        .find(|child| child.has_tag_name((DC, "creator")) && creator_role(node, *child) == "aut");
    let author_sort = first_creator
        .and_then(|creator| creator.attribute((OPF, "file-as")))
        .map(str::to_owned)
        .or_else(|| {
            first_creator
                .and_then(|creator| creator.attribute("id"))
                .and_then(|id| {
                    node.children()
                        .find(|child| {
                            child.is_element()
                                && child.attribute("property") == Some("file-as")
                                && child
                                    .attribute("refines")
                                    .map(|value| value.trim_start_matches('#'))
                                    == Some(id)
                        })
                        .map(node_text)
                })
        })
        .unwrap_or_else(|| authors.first().cloned().unwrap_or_default());
    let (series, series_index) = read_series(node);
    let isbn = node
        .children()
        .filter(|child| child.has_tag_name((DC, "identifier")))
        .filter(|child| is_isbn(*child))
        .map(node_text)
        .next_back()
        .map(|value| value.strip_prefix("urn:isbn:").unwrap_or(&value).to_owned());
    BookMetadata {
        title: dc_values(node, "title")
            .into_iter()
            .next()
            .unwrap_or_else(|| fallback.into()),
        authors,
        author_sort,
        series,
        series_index,
        genres: dc_values(node, "subject"),
        tags: node
            .children()
            .find(|child| {
                child.is_element() && child.attribute("name") == Some("library-manager:tags")
            })
            .and_then(|child| child.attribute("content"))
            .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
            .unwrap_or_default(),
        language: dc_values(node, "language")
            .into_iter()
            .next()
            .unwrap_or_else(|| "und".into()),
        description: dc_values(node, "description").join("\n"),
        isbn,
        publisher: dc_values(node, "publisher").into_iter().next(),
        published: dc_values(node, "date").into_iter().next(),
    }
}

fn read_series(node: Node<'_, '_>) -> (Option<String>, Option<f64>) {
    let calibre = node
        .children()
        .find(|child| child.is_element() && child.attribute("name") == Some("calibre:series"));
    let collection = node.children().find(|child| {
        child.is_element()
            && child.attribute("property") == Some("belongs-to-collection")
            && is_series_collection(node, *child)
    });
    let series = calibre
        .and_then(|child| child.attribute("content"))
        .map(str::to_owned)
        .or_else(|| collection.map(node_text));
    let index = node
        .children()
        .find(|child| child.is_element() && child.attribute("name") == Some("calibre:series_index"))
        .and_then(|child| child.attribute("content"))
        .map(str::to_owned)
        .or_else(|| {
            collection
                .and_then(|child| child.attribute("id"))
                .and_then(|id| {
                    node.children().find(|child| {
                        child.is_element()
                            && child.attribute("property") == Some("group-position")
                            && child
                                .attribute("refines")
                                .map(|value| value.trim_start_matches('#'))
                                == Some(id)
                    })
                })
                .map(node_text)
        })
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0);
    (series.filter(|value| !value.is_empty()), index)
}

fn is_series_collection(metadata: Node<'_, '_>, collection: Node<'_, '_>) -> bool {
    let Some(id) = collection.attribute("id") else {
        return false;
    };
    metadata.children().any(|node| {
        node.is_element()
            && node.attribute("property") == Some("collection-type")
            && node
                .attribute("refines")
                .map(|value| value.trim_start_matches('#'))
                == Some(id)
            && node_text(node) == "series"
    })
}

fn is_isbn(node: Node<'_, '_>) -> bool {
    node.attribute((OPF, "scheme"))
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("isbn"))
        || node_text(node).to_lowercase().starts_with("urn:isbn:")
}

fn find_cover(metadata: Node<'_, '_>, manifest: &[ManifestItem]) -> Option<String> {
    manifest
        .iter()
        .find(|item| {
            item.properties
                .split_whitespace()
                .any(|value| value == "cover-image")
        })
        .or_else(|| {
            metadata
                .children()
                .find(|node| node.is_element() && node.attribute("name") == Some("cover"))
                .and_then(|node| node.attribute("content"))
                .and_then(|id| manifest.iter().find(|item| item.id == id))
        })
        .map(|item| item.href.clone())
}

fn read_toc(
    entries: &BTreeMap<String, Vec<u8>>,
    manifest: &[ManifestItem],
    spine: &[String],
) -> Vec<TocEntry> {
    if let Some(nav) = manifest.iter().find(|item| {
        item.properties
            .split_whitespace()
            .any(|value| value == "nav")
    }) && let Ok(source) = entry_text(entries, &nav.href)
        && let Ok(document) = parse_markup(source)
        && let Some(node) = document.descendants().find(|node| {
            node.is_element()
                && node.tag_name().name() == "nav"
                && node.attributes().any(|attribute| {
                    attribute.name() == "type"
                        && attribute
                            .value()
                            .split_whitespace()
                            .any(|value| value == "toc")
                })
        })
        && let Some(list) = child_named(node, "ol")
    {
        let items = read_nav_list(list, &nav.href, 0);
        if !items.is_empty() {
            return items;
        }
    }
    if let Some(ncx) = manifest
        .iter()
        .find(|item| item.media_type == "application/x-dtbncx+xml")
        && let Ok(source) = entry_text(entries, &ncx.href)
        && let Ok(document) = parse_markup(source)
        && let Some(map) = document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "navMap")
    {
        let items = read_ncx_list(map, &ncx.href, 0);
        if !items.is_empty() {
            return items;
        }
    }
    spine
        .iter()
        .enumerate()
        .map(|(index, href)| TocEntry {
            label: format!("{}", index + 1),
            href: href.clone(),
            children: Vec::new(),
        })
        .collect()
}

fn toc_href(base: &str, href: &str) -> Result<String> {
    let path = resolve_href(base, href)?;
    match href.split_once('#') {
        Some((_, fragment)) => Ok(format!("{path}#{}", percent_decode(fragment)?)),
        None => Ok(path),
    }
}

fn read_nav_list(list: Node<'_, '_>, base: &str, depth: usize) -> Vec<TocEntry> {
    if depth > 32 {
        return Vec::new();
    }
    list.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "li")
        .filter_map(|node| {
            let anchor = child_named(node, "a")?;
            let href = toc_href(base, anchor.attribute("href")?).ok()?;
            Some(TocEntry {
                label: node_text(anchor),
                href,
                children: child_named(node, "ol")
                    .map(|list| read_nav_list(list, base, depth + 1))
                    .unwrap_or_default(),
            })
        })
        .collect()
}

fn read_ncx_list(parent: Node<'_, '_>, base: &str, depth: usize) -> Vec<TocEntry> {
    if depth > 32 {
        return Vec::new();
    }
    parent
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "navPoint")
        .filter_map(|node| {
            let content = child_named(node, "content")?;
            let href = toc_href(base, content.attribute("src")?).ok()?;
            Some(TocEntry {
                label: child_named(node, "navLabel")
                    .map(node_text)
                    .unwrap_or_default(),
                href,
                children: read_ncx_list(node, base, depth + 1),
            })
        })
        .collect()
}

fn validate_metadata(metadata: &BookMetadata) -> Result<()> {
    if metadata.title.trim().is_empty()
        || metadata.title.len() > 4096
        || metadata.authors.len() > 64
        || metadata
            .authors
            .iter()
            .any(|author| author.trim().is_empty() || author.len() > 4096)
        || metadata.description.len() > 1024 * 1024
        || metadata.genres.len() > 256
        || metadata.tags.len() > 256
        || metadata
            .genres
            .iter()
            .chain(&metadata.tags)
            .any(|value| value.len() > 4096)
        || metadata.author_sort.len() > 4096
        || metadata
            .series
            .as_ref()
            .is_some_and(|value| value.len() > 4096)
        || metadata
            .publisher
            .as_ref()
            .is_some_and(|value| value.len() > 4096)
        || metadata
            .isbn
            .as_ref()
            .is_some_and(|value| value.len() > 128)
        || metadata
            .published
            .as_ref()
            .is_some_and(|value| value.len() > 128)
        || metadata.language.is_empty()
        || metadata.language.len() > 64
        || metadata
            .series_index
            .is_some_and(|index| !index.is_finite() || index < 0.0)
    {
        return Err(invalid("Bibliographic metadata is invalid or too large"));
    }
    Ok(())
}

fn managed_node(metadata: Node<'_, '_>, node: Node<'_, '_>, unique_id: Option<&str>) -> bool {
    if !node.is_element() {
        return false;
    }
    if node.tag_name().namespace() == Some(DC) {
        return match node.tag_name().name() {
            "title" | "language" | "subject" | "description" | "publisher" | "date" => true,
            "creator" => creator_role(metadata, node) == "aut",
            "identifier" => {
                is_isbn(node) && unique_id.is_none_or(|id| node.attribute("id") != Some(id))
            }
            _ => false,
        };
    }
    node.tag_name().name() == "meta"
        && (matches!(
            node.attribute("name"),
            Some("calibre:series" | "calibre:series_index" | "library-manager:tags")
        ) || node.attribute("property") == Some("dcterms:modified")
            || (node.attribute("property") == Some("belongs-to-collection")
                && is_series_collection(metadata, node)))
}

fn rewrite_metadata(
    source: &str,
    package: Node<'_, '_>,
    node: Node<'_, '_>,
    metadata: &BookMetadata,
) -> Result<String> {
    if node
        .lookup_namespace_uri(Some("dc"))
        .is_some_and(|namespace| namespace != DC)
        || node
            .lookup_namespace_uri(Some("opf"))
            .is_some_and(|namespace| namespace != OPF)
    {
        return Err(AppError::Unsupported(
            "Conflicting metadata namespace prefixes cannot be rewritten safely".into(),
        ));
    }
    let removed_ids: HashSet<_> = node
        .children()
        .filter(|child| managed_node(node, *child, package.attribute("unique-identifier")))
        .filter_map(|child| child.attribute("id"))
        .collect();
    let mut preserved = String::new();
    for child in node.children() {
        let refinement_removed = child.is_element()
            && child
                .attribute("refines")
                .is_some_and(|value| removed_ids.contains(value.trim_start_matches('#')));
        if managed_node(node, child, package.attribute("unique-identifier")) || refinement_removed {
            continue;
        }
        let original = &source[child.range()];
        if child.has_tag_name((DC, "creator")) && creator_role(node, child) != "aut" {
            preserved.push_str(&as_contributor(original)?);
        } else {
            preserved.push_str(original);
        }
    }
    let mut used_ids: HashSet<String> = package
        .descendants()
        .filter_map(|child| child.attribute("id"))
        .filter(|id| !removed_ids.contains(id))
        .map(str::to_owned)
        .collect();
    let raw = &source[node.range()];
    let tag_end = opening_tag_end(raw)?;
    let qualified_name = raw[1..]
        .split(|character: char| character.is_whitespace() || matches!(character, '/' | '>'))
        .next()
        .ok_or_else(|| invalid("Invalid metadata element name"))?;
    let binding = regex::Regex::new(r#"\s+xmlns:(?:dc|opf)\s*=\s*(?:"[^"]*"|'[^']*')"#)
        .map_err(|_| invalid("Invalid namespace expression"))?;
    let cleaned = binding.replace_all(&raw[..tag_end], "");
    let opening = cleaned.trim_end().trim_end_matches('/');
    let mut output = format!("{opening} xmlns:dc=\"{DC}\" xmlns:opf=\"{OPF}\">{preserved}");
    output.push_str(&format!(
        "<dc:title>{}</dc:title><dc:language>{}</dc:language>",
        escape_xml(&metadata.title),
        escape_xml(&metadata.language)
    ));
    let epub3 = package
        .attribute("version")
        .is_some_and(|version| version.starts_with('3'));
    for (index, author) in metadata.authors.iter().enumerate() {
        let id = fresh_id(&format!("lm-author-{index}"), &mut used_ids);
        let sort = if index == 0 {
            metadata.author_sort.as_str()
        } else {
            author.as_str()
        };
        output.push_str(&format!(
            "<dc:creator id=\"{id}\" opf:role=\"aut\" opf:file-as=\"{}\">{}</dc:creator>",
            escape_xml(sort),
            escape_xml(author)
        ));
        if epub3 {
            output.push_str(&format!("<meta refines=\"#{id}\" property=\"role\" scheme=\"marc:relators\">aut</meta><meta refines=\"#{id}\" property=\"file-as\">{}</meta>", escape_xml(sort)));
        }
    }
    for genre in &metadata.genres {
        output.push_str(&format!("<dc:subject>{}</dc:subject>", escape_xml(genre)));
    }
    if !metadata.tags.is_empty() {
        let tags = serde_json::to_string(&metadata.tags)?;
        output.push_str(&format!(
            "<meta name=\"library-manager:tags\" content=\"{}\"/>",
            escape_xml(&tags)
        ));
    }
    for (name, value) in [
        ("description", Some(metadata.description.as_str())),
        ("publisher", metadata.publisher.as_deref()),
        ("date", metadata.published.as_deref()),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            output.push_str(&format!("<dc:{name}>{}</dc:{name}>", escape_xml(value)));
        }
    }
    if let Some(isbn) = metadata.isbn.as_deref().filter(|value| !value.is_empty()) {
        let id = fresh_id("lm-isbn", &mut used_ids);
        output.push_str(&format!(
            "<dc:identifier id=\"{id}\">urn:isbn:{}</dc:identifier>",
            escape_xml(isbn)
        ));
    }
    if let Some(series) = metadata.series.as_deref().filter(|value| !value.is_empty()) {
        output.push_str(&format!(
            "<meta name=\"calibre:series\" content=\"{}\"/>",
            escape_xml(series)
        ));
        if let Some(index) = metadata.series_index {
            output.push_str(&format!(
                "<meta name=\"calibre:series_index\" content=\"{index}\"/>"
            ));
        }
        if epub3 {
            let id = fresh_id("lm-series", &mut used_ids);
            output.push_str(&format!("<meta id=\"{id}\" property=\"belongs-to-collection\">{}</meta><meta refines=\"#{id}\" property=\"collection-type\">series</meta>", escape_xml(series)));
            if let Some(index) = metadata.series_index {
                output.push_str(&format!(
                    "<meta refines=\"#{id}\" property=\"group-position\">{index}</meta>"
                ));
            }
        }
    }
    if epub3 {
        output.push_str(&format!(
            "<meta property=\"dcterms:modified\">{}</meta>",
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        ));
    }
    output.push_str(&format!("</{qualified_name}>"));
    Ok(output)
}

fn opening_tag_end(source: &str) -> Result<usize> {
    let mut quote = None;
    for (index, character) in source.char_indices() {
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character == '>' {
            return Ok(index);
        }
    }
    Err(invalid("Unterminated XML opening tag"))
}

fn as_contributor(source: &str) -> Result<String> {
    let end = opening_tag_end(source)?;
    let name_end = source[1..end]
        .find(char::is_whitespace)
        .map(|index| index + 1)
        .unwrap_or(end);
    let qname = &source[1..name_end];
    let prefix = qname
        .rsplit_once(':')
        .map(|(prefix, _)| format!("{prefix}:"))
        .unwrap_or_default();
    let target = format!("{prefix}contributor");
    let opening = source.replacen(&format!("<{qname}"), &format!("<{target}"), 1);
    Ok(opening.replace(&format!("</{qname}>"), &format!("</{target}>")))
}

fn fresh_id(base: &str, used: &mut HashSet<String>) -> String {
    if used.insert(base.into()) {
        return base.into();
    }
    let mut suffix = 1_u64;
    loop {
        let id = format!("{base}-{suffix}");
        if used.insert(id.clone()) {
            return id;
        }
        suffix += 1;
    }
}

pub fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn write_zip(file: File, entries: &BTreeMap<String, Vec<u8>>, level: u8) -> Result<()> {
    let mut writer = ZipWriter::new(file);
    writer.start_file(
        "mimetype",
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
    )?;
    writer.write_all(EPUB_MIMETYPE)?;
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(i64::from(level)))
        .unix_permissions(0o600);
    for (name, bytes) in entries {
        if name != "mimetype" {
            writer.start_file(name, options)?;
            writer.write_all(bytes)?;
        }
    }
    writer.finish()?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fixture(version: &str, metadata: &str) -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            ("mimetype".into(), EPUB_MIMETYPE.to_vec()),
            ("META-INF/container.xml".into(), br#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OPS/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_vec()),
            ("OPS/book.opf".into(), format!(r#"<package xmlns="{OPF}" xmlns:d="{DC}" xmlns:opf="{OPF}" version="{version}" unique-identifier="uid"><metadata><d:identifier id="uid">urn:uuid:stable</d:identifier>{metadata}<!-- keep comment --><meta name="custom:field" content="keep"/></metadata><manifest><item id="c" href="chapter%20one.xhtml" media-type="application/xhtml+xml"/><item id="cover" href="cover.png" media-type="image/png" properties="cover-image"/></manifest><spine><itemref idref="c"/></spine></package>"#).into_bytes()),
            ("OPS/chapter one.xhtml".into(), br#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Chapter</title></head><body><h1>One</h1><p>All text &amp; punctuation stays.</p></body></html>"#.to_vec()),
            ("OPS/cover.png".into(), vec![1, 2, 3]),
        ])
    }

    fn edition_fixture(landmarks: bool) -> EpubDocument {
        let mut entries = fixture(
            "3.0",
            "<d:title>Stored title</d:title><d:language>fr</d:language>",
        );
        let extra_manifest = r#"<item id="title" href="Text/front.xhtml" media-type="application/xhtml+xml"/><item id="rights" href="Text/legal%20page.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>"#;
        let mut opf = entry_text(&entries, "OPS/book.opf")
            .unwrap()
            .replace("</manifest>", &format!("{extra_manifest}</manifest>"));
        if !landmarks {
            opf = opf.replace("</package>", r#"<guide><reference type="title-page" href="Text/front.xhtml"/><reference type="copyright-page" href="Text/legal%20page.xhtml#rights"/></guide></package>"#);
        }
        entries.insert("OPS/book.opf".into(), opf.into_bytes());
        entries.insert(
            "OPS/Text/front.xhtml".into(),
            b"<html><body>TITLE EDITION FRANCAISE</body></html>".to_vec(),
        );
        entries.insert(
            "OPS/Text/legal page.xhtml".into(),
            b"<html><body>COPYRIGHT 2024 ISBN 9780306406157</body></html>".to_vec(),
        );
        entries.insert("OPS/nav.xhtml".into(), br#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="landmarks"><a epub:type="titlepage" href="Text/front.xhtml">Title</a><a epub:type="copyright-page" href="Text/legal%20page.xhtml#rights">Rights</a></nav></body></html>"#.to_vec());
        entries.insert(
            "OPS/chapter one.xhtml".into(),
            format!(
                "<html><body>CHAPTER NARRATIVE {}</body></html>",
                "long chapter ".repeat(1000)
            )
            .into_bytes(),
        );
        EpubDocument::from_entries(entries, "Fallback").unwrap()
    }

    #[test]
    fn edition_sample_prioritizes_declared_frontmatter_outside_the_spine_and_reserves_a_chapter() {
        for landmarks in [false, true] {
            let document = edition_fixture(landmarks);
            let sample = document.edition_sample(240).unwrap();
            assert!(sample.contains("TITLE EDITION FRANCAISE"));
            assert!(sample.contains("COPYRIGHT 2024 ISBN 9780306406157"));
            assert!(sample.contains("CHAPTER NARRATIVE"));
            assert!(sample.chars().count() <= 240);
            assert_eq!(sample, document.edition_sample(240).unwrap());
            assert_eq!(document.spine, ["OPS/chapter one.xhtml"]);
        }
    }

    #[test]
    fn edition_sample_finds_late_copyright_without_spending_its_budget_on_the_first_chapter() {
        let mut document = edition_fixture(false);
        let opf = entry_text(&document.entries, &document.opf_path)
            .unwrap()
            .replace(
                "<itemref idref=\"c\"/>",
                "<itemref idref=\"c\"/><itemref idref=\"rights\"/>",
            );
        document
            .entries
            .insert(document.opf_path.clone(), opf.into_bytes());
        let document = EpubDocument::from_entries(document.entries, "Fallback").unwrap();
        let sample = document.edition_sample(180).unwrap();
        assert!(sample.contains("COPYRIGHT 2024 ISBN"));
        assert!(sample.contains("CHAPTER NARRATIVE"));
        assert!(sample.chars().count() <= 180);
    }

    #[test]
    fn edition_sample_rejects_unsafe_declared_paths_and_invalid_utf8() {
        for path in [
            "../../escape.xhtml",
            "https://example.org/page",
            "%2fetc/passwd",
            "Text/missing.xhtml",
        ] {
            let mut document = edition_fixture(false);
            let opf = entry_text(&document.entries, &document.opf_path)
                .unwrap()
                .replace("href=\"Text/front.xhtml\"/>", &format!("href=\"{path}\"/>"));
            document
                .entries
                .insert(document.opf_path.clone(), opf.into_bytes());
            assert!(document.edition_sample(240).is_err());
        }
        let mut document = edition_fixture(false);
        document
            .entries
            .insert("OPS/Text/front.xhtml".into(), vec![0xff, 0xfe]);
        assert!(document.edition_sample(240).is_err());
    }

    #[test]
    fn edition_sample_bounds_utf8_text_and_skips_oversized_pages_without_mutating_the_book() {
        let mut document = edition_fixture(false);
        document.entries.insert(
            "OPS/Text/front.xhtml".into(),
            "<html><body>Édition 日本語</body></html>"
                .as_bytes()
                .to_vec(),
        );
        document.entries.insert(
            "OPS/Text/legal page.xhtml".into(),
            vec![b' '; 512 * 1024 + 1],
        );
        let original = document.entries.clone();
        for limit in [0, 1, 2, 17, 240, usize::MAX] {
            let sample = document.edition_sample(limit).unwrap();
            assert!(sample.chars().count() <= limit.min(MAX_INSPECTION_SAMPLE_CHARACTERS));
        }
        assert_eq!(document.edition_sample(1).unwrap(), "É");
        let sample = document.edition_sample(240).unwrap();
        assert!(sample.contains("Édition 日本語"));
        assert!(sample.contains("CHAPTER NARRATIVE"));
        assert_eq!(document.entries, original);
    }

    #[test]
    fn edition_sample_keeps_a_plain_chapter_when_no_frontmatter_is_declared() {
        let document =
            EpubDocument::from_entries(fixture("3.0", "<d:title>Book</d:title>"), "Fallback")
                .unwrap();
        let sample = document.edition_sample(120).unwrap();
        assert!(sample.contains("All text & punctuation stays."));
        assert!(sample.chars().count() <= 120);
    }

    #[test]
    fn edition_sample_rejects_entity_declarations_and_ignores_script_content() {
        let mut document = edition_fixture(false);
        document.entries.insert("OPS/Text/front.xhtml".into(), br#"<!DOCTYPE html [<!ENTITY private SYSTEM "file:///etc/passwd">]><html><body>&private;</body></html>"#.to_vec());
        assert!(document.edition_sample(240).is_err());
        document.entries.insert("OPS/Text/front.xhtml".into(), b"<html><body>TITLE EDITION<script>PRIVATE_SCRIPT</script><style>PRIVATE_STYLE</style></body></html>".to_vec());
        let sample = document.edition_sample(240).unwrap();
        assert!(sample.contains("TITLE EDITION"));
        assert!(!sample.contains("PRIVATE_SCRIPT"));
        assert!(!sample.contains("PRIVATE_STYLE"));
    }

    #[test]
    fn inventory_reads_metadata_without_opening_chapters_or_images() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("inventory.epub");
        let entries = fixture(
            "3.0",
            "<d:title>Catalogue</d:title><d:creator>Auteur</d:creator><d:language>fr</d:language>",
        );
        write_zip(File::create(&path).unwrap(), &entries, 6).unwrap();
        let metadata = read_inventory_metadata(&path).unwrap();
        assert_eq!(
            read_inventory_metadata_file(File::open(&path).unwrap(), "inventory").unwrap(),
            metadata
        );
        assert_eq!(metadata, EpubDocument::open(&path).unwrap().metadata);
        assert_eq!(metadata.title, "Catalogue");
        assert_eq!(metadata.authors, ["Auteur"]);
        assert_eq!(metadata.language, "fr");

        // A damaged resource must not force a catalogue scan to transfer its bytes.
        let mut archive = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let offset = archive
            .by_name("OPS/cover.png")
            .unwrap()
            .data_start()
            .unwrap() as usize;
        drop(archive);
        let mut bytes = fs::read(&path).unwrap();
        bytes[offset] ^= 0xff;
        fs::write(&path, bytes).unwrap();
        assert_eq!(
            read_inventory_metadata_file(File::open(&path).unwrap(), "inventory").unwrap(),
            metadata
        );
        assert!(inspect_for_import(&path).is_err());

        let mut oversized = entries;
        oversized.insert(
            "OPS/chapter one.xhtml".into(),
            vec![b'x'; MAX_ENTRY_BYTES as usize + 1],
        );
        write_zip(File::create(&path).unwrap(), &oversized, 9).unwrap();
        assert_eq!(
            read_inventory_metadata_file(File::open(&path).unwrap(), "inventory").unwrap(),
            metadata
        );
        assert!(inspect_for_import(&path).is_err());
    }

    #[test]
    fn inventory_open_file_uses_caller_fallback_and_refuses_nonregular_sources() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("original filename.epub");
        let entries = fixture("3.0", "<d:language>fr</d:language>");
        write_zip(File::create(&path).unwrap(), &entries, 6).unwrap();
        assert_eq!(
            read_inventory_metadata(&path).unwrap().title,
            "original filename"
        );
        assert_eq!(
            read_inventory_metadata_file(File::open(&path).unwrap(), "Titre fourni — 日本語")
                .unwrap()
                .title,
            "Titre fourni — 日本語"
        );
        #[cfg(unix)]
        assert!(
            read_inventory_metadata_file(File::open(directory.path()).unwrap(), "directory")
                .is_err()
        );
    }

    #[test]
    fn inventory_open_file_never_reopens_a_replaced_path() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("inventory.epub");
        let entries = fixture("3.0", "<d:title>Opened edition</d:title>");
        write_zip(File::create(&path).unwrap(), &entries, 6).unwrap();
        let file = File::open(&path).unwrap();
        fs::rename(&path, directory.path().join("retained.epub")).unwrap();
        fs::write(&path, b"replacement is not a ZIP").unwrap();
        assert_eq!(
            read_inventory_metadata_file(file, "inventory")
                .unwrap()
                .title,
            "Opened edition"
        );
        assert!(read_inventory_metadata(&path).is_err());
    }

    #[test]
    fn inventory_rejects_malformed_or_unbounded_metadata() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("inventory.epub");
        for package in [
            "<broken",
            "<package/>",
            &format!("<package xmlns=\"{OPF}\"/>"),
            &format!(
                "<package xmlns=\"{OPF}\"><metadata id=\"dup\"/><manifest id=\"dup\"/></package>"
            ),
            &format!(
                "<!DOCTYPE package [<!ENTITY unsafe 'x'>]><package xmlns=\"{OPF}\"><metadata/></package>"
            ),
        ] {
            let mut entries = fixture("3.0", "<d:title>Book</d:title>");
            entries.insert("OPS/book.opf".into(), package.as_bytes().to_vec());
            write_zip(File::create(&path).unwrap(), &entries, 6).unwrap();
            assert!(read_inventory_metadata_file(File::open(&path).unwrap(), "inventory").is_err());
        }
        let mut entries = fixture("3.0", "<d:title>Book</d:title>");
        entries.insert(
            "OPS/book.opf".into(),
            vec![b'x'; MAX_INVENTORY_XML_BYTES as usize + 1],
        );
        write_zip(File::create(&path).unwrap(), &entries, 9).unwrap();
        assert!(read_inventory_metadata_file(File::open(&path).unwrap(), "inventory").is_err());
    }

    #[test]
    fn inventory_keeps_zip_path_and_metadata_integrity_guards() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("inventory.epub");
        for unsafe_name in ["../escaped.xhtml", "OPS/COVER.PNG"] {
            let mut entries = fixture("3.0", "<d:title>Book</d:title>");
            entries.insert(unsafe_name.into(), vec![1, 2, 3]);
            write_zip(File::create(&path).unwrap(), &entries, 6).unwrap();
            assert!(read_inventory_metadata_file(File::open(&path).unwrap(), "inventory").is_err());
        }
        let entries = fixture("3.0", "<d:title>Book</d:title>");
        write_zip(File::create(&path).unwrap(), &entries, 6).unwrap();
        let mut archive = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let offset = archive
            .by_name("OPS/book.opf")
            .unwrap()
            .data_start()
            .unwrap() as usize;
        drop(archive);
        let mut bytes = fs::read(&path).unwrap();
        bytes[offset] ^= 0xff;
        fs::write(&path, bytes).unwrap();
        assert!(read_inventory_metadata_file(File::open(&path).unwrap(), "inventory").is_err());
    }

    #[test]
    fn byte_reader_has_the_same_archive_checks_as_file_reader() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("source.epub");
        EpubDocument::from_entries(fixture("3.0", "<d:title>Byte reader</d:title>"), "Fallback")
            .unwrap()
            .write(&path, 9)
            .unwrap();
        let bytes = fs::read(&path).unwrap();
        let from_bytes = EpubDocument::from_bytes(&bytes, "Fallback").unwrap();
        let from_file = EpubDocument::open(&path).unwrap();
        assert_eq!(from_bytes.entries, from_file.entries);
        assert_eq!(from_bytes.metadata, from_file.metadata);
        assert_eq!(
            from_bytes.text_fingerprint().unwrap(),
            from_file.text_fingerprint().unwrap()
        );
        assert!(EpubDocument::from_bytes(&bytes[..bytes.len() / 2], "Fallback").is_err());
        assert!(EpubDocument::from_bytes(&bytes, &"x".repeat(4097)).is_err());
        let mut corrupted = bytes;
        corrupted[0] = 0;
        assert!(EpubDocument::from_bytes(&corrupted, "Fallback").is_err());
    }

    #[test]
    fn epub2_roles_zero_index_and_metadata_rewrite_preserve_resources() {
        let entries = fixture(
            "2.0",
            r#"<d:title>Old</d:title><d:creator opf:role="aut" opf:file-as="Writer, One">One Writer</d:creator><d:creator opf:role="trl">Translator</d:creator><meta name="calibre:series" content="Saga"/><meta name="calibre:series_index" content="0"/>"#,
        );
        let mut document = EpubDocument::from_entries(entries.clone(), "Fallback").unwrap();
        assert_eq!(document.metadata.authors, ["One Writer"]);
        assert_eq!(document.metadata.series_index, Some(0.0));
        let fingerprint = document.text_fingerprint().unwrap();
        let metadata = BookMetadata {
            title: "Correct & complete".into(),
            authors: vec!["New Writer".into()],
            author_sort: "Writer, New".into(),
            series: Some("Saga".into()),
            series_index: Some(3.5),
            genres: vec!["Fantasy".into()],
            tags: vec!["découverte".into()],
            language: "fr".into(),
            ..Default::default()
        };
        document.update_metadata(&metadata).unwrap();
        for (path, bytes) in &entries {
            if path != &document.opf_path {
                assert_eq!(&document.entries[path], bytes);
            }
        }
        assert_eq!(document.text_fingerprint().unwrap(), fingerprint);
        let opf = entry_text(&document.entries, &document.opf_path).unwrap();
        assert!(opf.contains("<!-- keep comment -->") && opf.contains("custom:field"));
        assert!(opf.contains("<d:contributor opf:role=\"trl\">Translator</d:contributor>"));
        let reloaded = EpubDocument::from_entries(document.entries, "Fallback").unwrap();
        assert_eq!(reloaded.metadata.authors, ["New Writer"]);
        assert_eq!(reloaded.metadata.series_index, Some(3.5));
        assert_eq!(reloaded.metadata.title, "Correct & complete");
        assert_eq!(reloaded.metadata.tags, ["découverte"]);
        assert_eq!(reloaded.metadata.genres, ["Fantasy"]);
        assert_eq!(reloaded.cover_path.as_deref(), Some("OPS/cover.png"));
    }

    #[test]
    fn epub3_refined_roles_series_and_uid_survive_roundtrip() {
        let mut document = EpubDocument::from_entries(fixture("3.0", r##"<d:title>Book</d:title><d:creator id="writer">Writer</d:creator><meta refines="#writer" property="role">aut</meta><d:creator id="translator">Translator</d:creator><meta refines="#translator" property="role">trl</meta><meta id="series" property="belongs-to-collection">Saga</meta><meta refines="#series" property="collection-type">series</meta><meta refines="#series" property="group-position">3.5</meta>"##), "Fallback").unwrap();
        assert_eq!(document.metadata.series_index, Some(3.5));
        let metadata = document.metadata.clone();
        document.update_metadata(&metadata).unwrap();
        let opf = entry_text(&document.entries, &document.opf_path).unwrap();
        assert!(opf.contains("urn:uuid:stable"));
        assert!(opf.contains("dcterms:modified"));
        assert!(opf.contains("id=\"translator\""));
        assert!(!opf.contains("refines=\"#writer\""));
        let parsed = EpubDocument::from_entries(document.entries, "Fallback").unwrap();
        assert_eq!(parsed.metadata.authors, ["Writer"]);
        assert_eq!(parsed.metadata.series_index, Some(3.5));
    }

    #[test]
    fn new_zip_has_stored_first_mimetype_and_crc_checked_content() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("book.epub");
        let document =
            EpubDocument::from_entries(fixture("3.0", "<d:title>Test</d:title>"), "Fallback")
                .unwrap();
        document.write(&path, 9).unwrap();
        let mut archive = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let first = archive.by_index(0).unwrap();
        assert_eq!(first.name(), "mimetype");
        assert_eq!(first.compression(), CompressionMethod::Stored);
        let start = first.data_start().unwrap() as usize;
        drop(first);
        assert_eq!(
            EpubDocument::open(&path)
                .unwrap()
                .text_fingerprint()
                .unwrap(),
            document.text_fingerprint().unwrap()
        );
        assert!(document.write(&path, 6).is_err());
        let mut corrupted = fs::read(&path).unwrap();
        corrupted[start] ^= 1;
        fs::write(&path, corrupted).unwrap();
        assert!(EpubDocument::open(&path).is_err());
    }

    #[test]
    fn unsafe_paths_dtd_duplicate_manifest_and_drm_are_rejected() {
        assert!(resolve_href("OPS/book.opf", "../../escape").is_err());
        assert!(resolve_href("OPS/book.opf", "%2fetc/passwd").is_err());
        assert!(resolve_href("OPS/book.opf", "https://example.org/x").is_err());
        assert!(validate_archive_name("../escape").is_err());
        assert!(validate_archive_name("a\\b").is_err());
        let mut entries = fixture("3.0", "<d:title>Book</d:title>");
        let opf = entry_text(&entries, "OPS/book.opf").unwrap().to_owned();
        entries.insert(
            "OPS/book.opf".into(),
            format!("<!DOCTYPE package [<!ENTITY bad 'unsafe'>]>{opf}").into_bytes(),
        );
        assert!(EpubDocument::from_entries(entries, "Fallback").is_err());
        let mut duplicate = fixture("3.0", "<d:title>Book</d:title>");
        let opf = entry_text(&duplicate, "OPS/book.opf")
            .unwrap()
            .replace("id=\"cover\"", "id=\"c\"");
        duplicate.insert("OPS/book.opf".into(), opf.into_bytes());
        assert!(EpubDocument::from_entries(duplicate, "Fallback").is_err());
        let mut encrypted = fixture("3.0", "<d:title>Book</d:title>");
        encrypted.insert("META-INF/encryption.xml".into(), br#"<encryption><EncryptedData><EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/></EncryptedData></encryption>"#.to_vec());
        assert!(EpubDocument::from_entries(encrypted, "Fallback").is_err());
    }

    #[test]
    fn obfuscated_fonts_do_not_count_as_drm_and_text_changes_are_detected() {
        let mut entries = fixture("3.0", "<d:title>Book</d:title>");
        entries.insert("OPS/font.otf".into(), vec![0; 1040]);
        entries.insert("META-INF/encryption.xml".into(), br#"<encryption><EncryptedData><EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/><CipherData><CipherReference URI="OPS/font.otf"/></CipherData></EncryptedData></encryption>"#.to_vec());
        let mut document = EpubDocument::from_entries(entries, "Fallback").unwrap();
        let before = document.text_fingerprint().unwrap();
        document.entries.insert(
            document.spine[0].clone(),
            b"<html><body>Different text</body></html>".to_vec(),
        );
        assert_ne!(before, document.text_fingerprint().unwrap());
    }

    #[test]
    fn conflicting_namespace_bindings_are_not_silently_reinterpreted() {
        let mut entries = fixture("3.0", "<d:title>Book</d:title>");
        let opf = entry_text(&entries, "OPS/book.opf")
            .unwrap()
            .replace("<metadata>", "<metadata xmlns:dc=\"urn:custom\">");
        entries.insert("OPS/book.opf".into(), opf.into_bytes());
        let mut document = EpubDocument::from_entries(entries.clone(), "Fallback").unwrap();
        assert!(
            document
                .update_metadata(&document.metadata.clone())
                .is_err()
        );
        assert_eq!(document.entries, entries);
    }

    #[test]
    fn standard_nbsp_is_decoded_without_changing_literal_sections_or_source() {
        let mut entries = fixture("3.0", "<d:title>Book</d:title>");
        let content = br#"<!DOCTYPE html SYSTEM "https://invalid.example/xhtml.dtd"><html><head><script>ignored&nbsp;</script><style>ignored&nbsp;</style></head><body><p>A&nbsp;B.</p><![CDATA[&nbsp; remains literal]]><p>&amp;nbsp; stays escaped.</p><!-- &nbsp; comment --><?keep &nbsp;?></body></html>"#.to_vec();
        entries.insert("OPS/chapter one.xhtml".into(), content.clone());
        let mut document = EpubDocument::from_entries(entries, "Fallback").unwrap();
        let before = document.text_fingerprint().unwrap();
        assert_eq!(
            document.spine_texts().unwrap(),
            ["A B. &nbsp; remains literal &nbsp; stays escaped."]
        );
        assert_eq!(document.entries["OPS/chapter one.xhtml"], content);
        document.normalize_standard_entities().unwrap();
        assert_eq!(document.text_fingerprint().unwrap(), before);
        let normalized = entry_text(&document.entries, "OPS/chapter one.xhtml").unwrap();
        assert!(normalized.contains("A&#160;B."));
        assert!(normalized.contains("<![CDATA[&nbsp; remains literal]]>"));
        assert!(normalized.contains("<!-- &nbsp; comment -->"));
        assert!(normalized.contains("<?keep &nbsp;?>"));
    }

    #[test]
    fn inspection_html_fallback_removes_active_content_and_keeps_word_boundaries() {
        let (text, fallback) = text_for_inspection(r#"<html><head><title>Hidden title</title><script>alert(1)</script><style>.bad{}</style></head><body><p>A&nbsp;B<img src=https://invalid.example/image><p>C &copy; D<br>E</body></html>"#).unwrap();
        assert!(fallback);
        assert_eq!(text, "A B C © D E");
        assert!(
            text_for_inspection(
                "<!DOCTYPE html [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><html>&x;</html>"
            )
            .is_err()
        );
        assert_eq!(
            text_for_inspection("<html><body>A&nbsp;B</body></html>").unwrap(),
            ("A B".into(), false)
        );
        assert_eq!(
            text_for_inspection("<html><body><![CDATA[<!ENTITY fake 'literal'>]]></body></html>")
                .unwrap(),
            ("<!ENTITY fake 'literal'>".into(), false)
        );
    }

    #[test]
    fn import_inspection_catalogues_missing_fonts_and_duplicate_unused_ids() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.epub");
        let mut entries = fixture(
            "3.0",
            "<d:title>Catalogue title</d:title><d:creator>Author</d:creator>",
        );
        let opf = entry_text(&entries, "OPS/book.opf").unwrap().replace("</manifest>", r#"<item id="font" href="missing.ttf" media-type="application/x-font-truetype"/><item id="cover" href="cover.png" media-type="image/png"/></manifest>"#);
        entries.insert("OPS/book.opf".into(), opf.into_bytes());
        write_zip(File::create(&path).unwrap(), &entries, 6).unwrap();
        assert!(EpubDocument::open(&path).is_err());
        let inspection = inspect_for_import(&path).unwrap();
        assert_eq!(inspection.metadata.title, "Catalogue title");
        assert_eq!(inspection.metadata.authors, ["Author"]);
        assert_eq!(inspection.chapter_count, 1);
        assert!(
            inspection
                .text_sample
                .contains("All text & punctuation stays.")
        );
        assert!(!inspection.transformable);
        assert!(
            inspection
                .warnings
                .iter()
                .any(|warning| warning.contains("missing resource"))
        );
        assert!(
            inspection
                .warnings
                .iter()
                .any(|warning| warning.contains("Duplicate manifest IDs"))
        );
    }

    #[test]
    fn external_content_doctype_is_compatible_without_entity_resolution() {
        let mut entries = fixture("2.0", "<d:title>Book</d:title>");
        let chapter = entry_text(&entries, "OPS/chapter one.xhtml")
            .unwrap()
            .to_owned();
        let mut document = EpubDocument::from_entries(entries.clone(), "Fallback").unwrap();
        let fingerprint = document.text_fingerprint().unwrap();
        entries.insert("OPS/chapter one.xhtml".into(), format!(r#"<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "https://invalid.example/never-fetched.dtd">{chapter}"#).into_bytes());
        document = EpubDocument::from_entries(entries, "Fallback").unwrap();
        assert_eq!(document.text_fingerprint().unwrap(), fingerprint);
        document.entries.insert(
            "OPS/chapter one.xhtml".into(),
            format!("<!DOCTYPE html [<!ENTITY external SYSTEM 'file:///etc/passwd'>]>{chapter}")
                .into_bytes(),
        );
        assert!(document.text_fingerprint().is_err());
        assert!(parse_markup("<!DOCTYPE html []><html/>").is_err());
        assert!(
            parse_markup(
                "<!DOCTYPE html SYSTEM 'https://invalid.example/x.dtd'><html>&external;</html>"
            )
            .is_err()
        );
    }
}
