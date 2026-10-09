//! EPUB variants with bounded raster decoding and verified chapter preservation.

use std::{collections::HashSet, fs, io::Cursor, ops::Range, path::Path};

use image::{
    DynamicImage, ImageFormat, ImageReader, Limits, codecs::jpeg::JpegEncoder, imageops::FilterType,
};
use roxmltree::Node;

use crate::{
    AppError, EpubDocument, OptimizationProfile, OptimizationReport, Result,
    epub::{escape_xml, parse_markup, resolve_href},
};

const MAX_IMAGE_PIXELS: u64 = 16_000_000;
const MAX_IMAGE_ALLOCATION: u64 = 128 * 1024 * 1024;

pub fn profiles() -> Vec<OptimizationProfile> {
    vec![
        OptimizationProfile {
            id: "lossless".into(),
            name: "Lossless".into(),
            compression_level: 9,
            jpeg_quality: 100,
            ..Default::default()
        },
        OptimizationProfile {
            id: "balanced".into(),
            name: "Balanced".into(),
            max_image_width: Some(1200),
            max_image_height: Some(1600),
            jpeg_quality: 85,
            compression_level: 9,
            ..Default::default()
        },
        OptimizationProfile {
            id: "xteink".into(),
            name: "Micro reader · 480 × 800".into(),
            max_image_width: Some(480),
            max_image_height: Some(800),
            jpeg_quality: 75,
            grayscale: true,
            remove_embedded_fonts: true,
            compression_level: 9,
            ..Default::default()
        },
        OptimizationProfile {
            id: "textOnly".into(),
            name: "Text only".into(),
            jpeg_quality: 85,
            remove_images: true,
            remove_embedded_fonts: true,
            compression_level: 9,
            ..Default::default()
        },
    ]
}

pub fn profile(id: &str) -> Result<OptimizationProfile> {
    profiles()
        .into_iter()
        .find(|profile| profile.id == id)
        .ok_or_else(|| AppError::NotFound("Optimization profile".into()))
}

pub fn optimize(
    source: &Path,
    destination: &Path,
    profile: &OptimizationProfile,
    book_id: &str,
    file_id: &str,
) -> Result<OptimizationReport> {
    validate_profile(profile)?;
    let mut document = EpubDocument::open(source)?;
    let before_text = document.text_fingerprint()?;
    let before_spine = document.spine.clone();
    if profile.remove_images || profile.remove_embedded_fonts {
        document.normalize_standard_entities()?;
    }
    let mut report = OptimizationReport {
        book_id: book_id.into(),
        file_id: file_id.into(),
        before_bytes: fs::metadata(source)?.len(),
        chapters_before: before_spine.len() as u64,
        ..Default::default()
    };
    if profile.simplify_css {
        report
            .warnings
            .push("General CSS simplification is disabled to preserve book layout".into());
    }
    if profile.remove_images {
        remove_images(&mut document, &mut report)?;
        document = EpubDocument::from_entries(document.entries, &document.metadata.title)?;
    } else if profile.max_image_width.is_some()
        || profile.max_image_height.is_some()
        || profile.grayscale
    {
        optimize_rasters(&mut document, profile, &mut report)?;
    }
    if profile.remove_embedded_fonts {
        remove_fonts(&mut document, &mut report)?;
    }
    let document = EpubDocument::from_entries(document.entries, &document.metadata.title)?;
    if document.spine != before_spine || document.text_fingerprint()? != before_text {
        return Err(AppError::Conflict(
            "Optimization would alter chapter order or text".into(),
        ));
    }
    document.write(destination, profile.compression_level)?;
    let result = verify_output(destination, &before_spine, &before_text);
    let reopened = match result {
        Ok(document) => document,
        Err(error) => {
            let _ = fs::remove_file(destination);
            return Err(error);
        }
    };
    report.after_bytes = fs::metadata(destination)?.len();
    report.chapters_after = reopened.spine.len() as u64;
    report.text_preserved = true;
    if report.after_bytes > report.before_bytes {
        report
            .warnings
            .push("The validated variant is larger than the source EPUB".into());
    }
    report.warnings.sort();
    report.warnings.dedup();
    Ok(report)
}

fn validate_profile(profile: &OptimizationProfile) -> Result<()> {
    if profile.jpeg_quality == 0
        || profile.jpeg_quality > 100
        || profile.compression_level > 9
        || profile.max_image_width == Some(0)
        || profile.max_image_height == Some(0)
    {
        return Err(AppError::InvalidInput(
            "Optimization profile limits are invalid".into(),
        ));
    }
    Ok(())
}

fn verify_output(path: &Path, spine: &[String], fingerprint: &str) -> Result<EpubDocument> {
    let document = EpubDocument::open(path)?;
    if document.spine != spine || document.text_fingerprint()? != fingerprint {
        return Err(AppError::Conflict(
            "Generated EPUB failed its text integrity check".into(),
        ));
    }
    Ok(document)
}

fn image_error() -> AppError {
    AppError::InvalidInput("Raster image is invalid or exceeds decoder safety limits".into())
}

fn limited_reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| image_error())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_PIXELS as u32);
    limits.max_image_height = Some(MAX_IMAGE_PIXELS as u32);
    limits.max_alloc = Some(MAX_IMAGE_ALLOCATION);
    reader.limits(limits);
    Ok(reader)
}

fn optimize_rasters(
    document: &mut EpubDocument,
    profile: &OptimizationProfile,
    report: &mut OptimizationReport,
) -> Result<()> {
    let images: Vec<_> = document
        .manifest
        .iter()
        .filter(|item| item.media_type.starts_with("image/"))
        .cloned()
        .collect();
    for item in images {
        if !matches!(item.media_type.as_str(), "image/jpeg" | "image/png") {
            report.warnings.push("Raster formats other than JPEG/PNG and vector images retain their original resources".into());
            continue;
        }
        let original = document.entries.get(&item.href).ok_or_else(image_error)?;
        let reader = limited_reader(original)?;
        let format = reader.format().ok_or_else(image_error)?;
        if !matches!(
            (item.media_type.as_str(), format),
            ("image/jpeg", ImageFormat::Jpeg) | ("image/png", ImageFormat::Png)
        ) {
            return Err(AppError::InvalidInput(
                "Image signature and manifest media type disagree".into(),
            ));
        }
        let (width, height) = reader.into_dimensions().map_err(|_| image_error())?;
        if u64::from(width)
            .checked_mul(u64::from(height))
            .is_none_or(|pixels| pixels == 0 || pixels > MAX_IMAGE_PIXELS)
        {
            return Err(image_error());
        }
        let target_width = profile.max_image_width.unwrap_or(width);
        let target_height = profile.max_image_height.unwrap_or(height);
        let resize = width > target_width || height > target_height;
        let mut image = limited_reader(original)?
            .decode()
            .map_err(|_| image_error())?;
        if resize {
            image = image.resize(target_width, target_height, FilterType::Lanczos3);
        }
        if profile.grayscale {
            image = image.grayscale();
        }
        let encoded = encode_image(&image, format, profile.jpeg_quality)?;
        if resize || encoded.len() < original.len() {
            if encoded != *original {
                document.entries.insert(item.href, encoded);
                report.images_changed += 1;
            }
        } else if profile.grayscale {
            report.warnings.push("Some small images retain their original colors because grayscale encoding would increase their size".into());
        }
    }
    Ok(())
}

fn encode_image(image: &DynamicImage, format: ImageFormat, quality: u8) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    if format == ImageFormat::Jpeg {
        JpegEncoder::new_with_quality(&mut output, quality)
            .encode_image(image)
            .map_err(|_| image_error())?;
    } else {
        image
            .write_to(&mut Cursor::new(&mut output), format)
            .map_err(|_| image_error())?;
    }
    Ok(output)
}

fn is_markup(media_type: &str) -> bool {
    matches!(
        media_type,
        "application/xhtml+xml" | "text/html" | "image/svg+xml" | "application/x-dtbncx+xml"
    )
}

fn is_font(media_type: &str, path: &str) -> bool {
    media_type.starts_with("font/")
        || media_type.contains("font")
        || media_type == "application/vnd.ms-opentype"
        || matches!(
            path.rsplit('.')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "ttf" | "otf" | "woff" | "woff2"
        )
}

fn remove_fonts(document: &mut EpubDocument, report: &mut OptimizationReport) -> Result<()> {
    let candidates: HashSet<_> = document
        .manifest
        .iter()
        .filter(|item| is_font(&item.media_type, &item.href))
        .map(|item| item.href.clone())
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }
    let images = HashSet::new();
    rewrite_resources(
        document,
        &RewriteOptions {
            images: &images,
            fonts: &candidates,
            remove_images: false,
            strip_fonts: true,
        },
    )?;
    let retained = referenced_resources(document, &candidates)?;
    let mut removed: HashSet<_> = candidates.difference(&retained).cloned().collect();
    if !retained.is_empty() {
        report.warnings.push(
            "Some embedded fonts are retained because other active resources still reference them"
                .into(),
        );
    }
    report.fonts_removed = removed.len() as u64;
    for path in &removed {
        document.entries.remove(path);
    }
    if remove_encryption_references(document, &removed)? {
        removed.insert("META-INF/encryption.xml".into());
    }
    remove_package_references(document, &removed)
}

fn remove_images(document: &mut EpubDocument, report: &mut OptimizationReport) -> Result<()> {
    let spine: HashSet<_> = document.spine.iter().cloned().collect();
    let candidates: HashSet<_> = document
        .manifest
        .iter()
        .filter(|item| item.media_type.starts_with("image/") && !spine.contains(&item.href))
        .map(|item| item.href.clone())
        .collect();
    if document
        .manifest
        .iter()
        .any(|item| item.media_type.starts_with("image/") && spine.contains(&item.href))
    {
        report.warnings.push("Vector pages in the reading order are retained to preserve their text and chapter structure".into());
    }
    let fonts = HashSet::new();
    rewrite_resources(
        document,
        &RewriteOptions {
            images: &candidates,
            fonts: &fonts,
            remove_images: true,
            strip_fonts: false,
        },
    )?;
    for path in &candidates {
        document.entries.remove(path);
    }
    report.images_removed = candidates.len() as u64;
    report.warnings.push(
        "Illustrations are removed; captions and accessible alternative descriptions are retained"
            .into(),
    );
    remove_package_references(document, &candidates)
}

fn source_text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes)
        .map_err(|_| AppError::Unsupported("Optimization requires UTF-8 styles and markup".into()))
}

struct RewriteOptions<'a> {
    images: &'a HashSet<String>,
    fonts: &'a HashSet<String>,
    remove_images: bool,
    strip_fonts: bool,
}

fn rewrite_resources(document: &mut EpubDocument, options: &RewriteOptions<'_>) -> Result<()> {
    let resources: Vec<_> = document
        .manifest
        .iter()
        .filter(|item| item.media_type == "text/css" || is_markup(&item.media_type))
        .cloned()
        .collect();
    for item in resources {
        if options.images.contains(&item.href) {
            continue;
        }
        let source = source_text(
            document
                .entries
                .get(&item.href)
                .ok_or_else(|| AppError::InvalidInput("Missing optimization resource".into()))?,
        )?;
        let rewritten = if item.media_type == "text/css" {
            rewrite_css(source, &item.href, options)?
        } else {
            rewrite_markup(source, &item.href, options)?
        };
        if rewritten != source {
            document.entries.insert(item.href, rewritten.into_bytes());
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Edit {
    range: Range<usize>,
    replacement: String,
}

fn add_edit(edits: &mut Vec<Edit>, range: Range<usize>, replacement: String) {
    if edits
        .iter()
        .any(|edit| edit.range.start <= range.start && edit.range.end >= range.end)
    {
        return;
    }
    edits.retain(|edit| !(range.start <= edit.range.start && range.end >= edit.range.end));
    edits.push(Edit { range, replacement });
}

fn apply_edits(source: &str, mut edits: Vec<Edit>) -> Result<String> {
    edits.sort_by_key(|edit| edit.range.start);
    if edits
        .windows(2)
        .any(|pair| pair[0].range.end > pair[1].range.start)
    {
        return Err(AppError::Conflict("Overlapping resource edits".into()));
    }
    let mut output = source.to_owned();
    for edit in edits.into_iter().rev() {
        output.replace_range(edit.range, &edit.replacement);
    }
    Ok(output)
}

fn matches_resource(base: &str, value: &str, paths: &HashSet<String>) -> bool {
    resolve_href(base, value).is_ok_and(|path| paths.contains(&path))
}

fn visible_text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|node| {
            node.is_text()
                && !node.ancestors().any(|ancestor| {
                    ancestor.is_element()
                        && matches!(ancestor.tag_name().name(), "script" | "style")
                })
        })
        .filter_map(|node| node.text())
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn text_only_replacement(node: Node<'_, '_>) -> String {
    let label = node
        .attribute("alt")
        .or_else(|| node.attribute("aria-label"))
        .or_else(|| node.descendants().find_map(|child| child.attribute("alt")))
        .unwrap_or_default();
    let text = visible_text(node);
    let tag = if node
        .parent_element()
        .is_some_and(|parent| parent.tag_name().namespace() == Some("http://www.w3.org/2000/svg"))
    {
        "g"
    } else {
        "span"
    };
    format!(
        "<{tag} aria-label=\"{}\">{}</{tag}>",
        escape_xml(label),
        escape_xml(&text)
    )
}

fn rewrite_markup(source: &str, base: &str, options: &RewriteOptions<'_>) -> Result<String> {
    let document = parse_markup(source)?;
    let mut edits = Vec::new();
    for node in document.descendants().filter(Node::is_element) {
        let tag = node.tag_name().name();
        let image_target = options.remove_images
            && (matches!(tag, "img" | "image" | "picture" | "source")
                || (tag == "link"
                    && node
                        .attribute("href")
                        .is_some_and(|href| matches_resource(base, href, options.images)))
                || (tag == "svg" && node != document.root_element())
                || (matches!(tag, "object" | "embed")
                    && node.attributes().any(|attribute| {
                        matches!(attribute.name(), "src" | "data")
                            && (matches_resource(base, attribute.value(), options.images)
                                || is_embedded_image(attribute.value()))
                    })));
        if image_target {
            let replacement = if tag == "link" {
                String::new()
            } else {
                text_only_replacement(node)
            };
            add_edit(&mut edits, node.range(), replacement);
            continue;
        }
        let font_target = matches!(tag, "link" | "font-face-uri")
            && node.attributes().any(|attribute| {
                attribute.name() == "href"
                    && matches_resource(base, attribute.value(), options.fonts)
            });
        if font_target {
            add_edit(&mut edits, node.range(), String::new());
            continue;
        }
        if tag == "style" {
            let css = node
                .children()
                .filter_map(|child| child.text())
                .collect::<String>();
            let updated = rewrite_css(&css, base, options)?;
            if updated != css {
                let raw = &source[node.range()];
                let opening_end = xml_opening_end(raw)?;
                let closing_start = raw.rfind("</").ok_or_else(|| {
                    AppError::InvalidInput("Style element has no closing tag".into())
                })?;
                add_edit(
                    &mut edits,
                    node.range(),
                    format!(
                        "{}{}{}",
                        &raw[..=opening_end],
                        escape_xml(&updated),
                        &raw[closing_start..]
                    ),
                );
            }
        }
        for attribute in node.attributes() {
            if attribute.name() == "style" {
                let updated = rewrite_css(attribute.value(), base, options)?;
                if updated != attribute.value() {
                    let qname = &source[attribute.range_qname()];
                    add_edit(
                        &mut edits,
                        attribute.range(),
                        format!("{qname}=\"{}\"", escape_xml(&updated)),
                    );
                }
            } else if options.remove_images
                && matches!(
                    attribute.name(),
                    "href" | "src" | "poster" | "background" | "data"
                )
                && (matches_resource(base, attribute.value(), options.images)
                    || is_embedded_image(attribute.value()))
            {
                add_edit(&mut edits, attribute.range(), String::new());
            }
        }
    }
    apply_edits(source, edits)
}

fn xml_opening_end(source: &str) -> Result<usize> {
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
    Err(AppError::InvalidInput("Unterminated XML element".into()))
}

fn referenced_resources(
    document: &EpubDocument,
    candidates: &HashSet<String>,
) -> Result<HashSet<String>> {
    let mut references = HashSet::new();
    for item in &document.manifest {
        if candidates.contains(&item.href) {
            continue;
        }
        let Some(bytes) = document.entries.get(&item.href) else {
            continue;
        };
        if item.media_type == "text/css" {
            collect_css_references(source_text(bytes)?, &item.href, candidates, &mut references)?;
        } else if is_markup(&item.media_type) {
            let markup = parse_markup(source_text(bytes)?)?;
            for node in markup.descendants().filter(Node::is_element) {
                for attribute in node.attributes() {
                    if matches!(attribute.name(), "src" | "href" | "data" | "poster") {
                        if let Ok(path) = resolve_href(&item.href, attribute.value())
                            && candidates.contains(&path)
                        {
                            references.insert(path);
                        }
                    } else if attribute.name() == "style" {
                        collect_css_references(
                            attribute.value(),
                            &item.href,
                            candidates,
                            &mut references,
                        )?;
                    }
                }
                if node.tag_name().name() == "style" {
                    let css = node
                        .children()
                        .filter_map(|child| child.text())
                        .collect::<String>();
                    collect_css_references(&css, &item.href, candidates, &mut references)?;
                }
            }
        }
    }
    Ok(references)
}

fn collect_css_references(
    css: &str,
    base: &str,
    candidates: &HashSet<String>,
    references: &mut HashSet<String>,
) -> Result<()> {
    for url in css_urls(css)? {
        if let Ok(path) = resolve_href(base, &url.value)
            && candidates.contains(&path)
        {
            references.insert(path);
        }
    }
    Ok(())
}

fn remove_package_references(document: &mut EpubDocument, paths: &HashSet<String>) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let source = source_text(&document.entries[&document.opf_path])?;
    let parsed = parse_markup(source)?;
    let removed_ids: HashSet<_> = parsed
        .descendants()
        .filter(|node| {
            node.is_element()
                && node.tag_name().name() == "item"
                && node
                    .attribute("href")
                    .is_some_and(|href| matches_resource(&document.opf_path, href, paths))
        })
        .filter_map(|node| node.attribute("id"))
        .collect();
    let mut edits = Vec::new();
    for node in parsed.descendants().filter(Node::is_element) {
        let removed_item = node.tag_name().name() == "item"
            && node
                .attribute("id")
                .is_some_and(|id| removed_ids.contains(id));
        let removed_cover = node.tag_name().name() == "meta"
            && node.attribute("name") == Some("cover")
            && node
                .attribute("content")
                .is_some_and(|id| removed_ids.contains(id));
        let removed_href = matches!(node.tag_name().name(), "reference" | "link")
            && node
                .attribute("href")
                .is_some_and(|href| matches_resource(&document.opf_path, href, paths));
        let removed_refinement = node.tag_name().name() == "meta"
            && node.attribute("refines").is_some_and(|target| {
                target
                    .strip_prefix('#')
                    .is_some_and(|id| removed_ids.contains(id))
            });
        let removed_binding = node.tag_name().name() == "mediaType"
            && node
                .attribute("handler")
                .is_some_and(|id| removed_ids.contains(id));
        if removed_item || removed_cover || removed_href || removed_refinement || removed_binding {
            add_edit(&mut edits, node.range(), String::new());
        } else {
            for attribute in node.attributes() {
                if matches!(attribute.name(), "fallback" | "fallback-style")
                    && removed_ids.contains(attribute.value())
                {
                    add_edit(&mut edits, attribute.range(), String::new());
                }
            }
        }
    }
    let rewritten = apply_edits(source, edits)?;
    document
        .entries
        .insert(document.opf_path.clone(), rewritten.into_bytes());
    Ok(())
}

fn remove_encryption_references(
    document: &mut EpubDocument,
    removed: &HashSet<String>,
) -> Result<bool> {
    let Some(bytes) = document.entries.get("META-INF/encryption.xml") else {
        return Ok(false);
    };
    let source = source_text(bytes)?;
    let parsed = parse_markup(source)?;
    let mut edits = Vec::new();
    let mut remaining = 0;
    for node in parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "EncryptedData")
    {
        let removable = node.descendants().any(|child| {
            child.is_element()
                && child.tag_name().name() == "CipherReference"
                && child
                    .attribute("URI")
                    .is_some_and(|uri| matches_resource("", uri, removed))
        });
        if removable {
            add_edit(&mut edits, node.range(), String::new());
        } else {
            remaining += 1;
        }
    }
    if edits.is_empty() {
        return Ok(false);
    }
    if remaining == 0 {
        document.entries.remove("META-INF/encryption.xml");
        return Ok(true);
    }
    let rewritten = apply_edits(source, edits)?;
    document
        .entries
        .insert("META-INF/encryption.xml".into(), rewritten.into_bytes());
    Ok(false)
}

#[derive(Debug)]
struct CssUrl {
    range: Range<usize>,
    value: String,
}

fn css_error() -> AppError {
    AppError::Unsupported("Malformed CSS cannot be rewritten safely".into())
}

fn skip_css_literal(bytes: &[u8], index: &mut usize) -> Result<bool> {
    if bytes.get(*index..*index + 2) == Some(b"/*") {
        let start = *index + 2;
        let end = bytes[start..]
            .windows(2)
            .position(|pair| pair == b"*/")
            .ok_or_else(css_error)?;
        *index = start + end + 2;
        return Ok(true);
    }
    if bytes
        .get(*index)
        .is_some_and(|byte| matches!(byte, b'\'' | b'"'))
    {
        let delimiter = bytes[*index];
        *index += 1;
        while *index < bytes.len() {
            if bytes[*index] == b'\\' {
                *index += 2;
            } else if bytes[*index] == delimiter {
                *index += 1;
                return Ok(true);
            } else {
                *index += 1;
            }
        }
        return Err(css_error());
    }
    Ok(false)
}

fn css_urls(css: &str) -> Result<Vec<CssUrl>> {
    let bytes = css.as_bytes();
    let mut index = 0;
    let mut urls = Vec::new();
    while index < bytes.len() {
        if skip_css_literal(bytes, &mut index)? {
            continue;
        }
        let word = bytes.get(index..index + 3);
        let boundary = index == 0
            || !matches!(bytes[index - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-');
        if boundary && word.is_some_and(|word| word.eq_ignore_ascii_case(b"url")) {
            let start = index;
            let mut cursor = index + 3;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            if bytes.get(cursor) != Some(&b'(') {
                index += 3;
                continue;
            }
            cursor += 1;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            let value_start;
            let value_end;
            if bytes
                .get(cursor)
                .is_some_and(|byte| matches!(byte, b'\'' | b'"'))
            {
                value_start = cursor + 1;
                skip_css_literal(bytes, &mut cursor)?;
                value_end = cursor - 1;
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
            } else {
                value_start = cursor;
                while cursor < bytes.len() && bytes[cursor] != b')' {
                    if bytes[cursor] == b'\\' {
                        cursor += 2;
                    } else {
                        cursor += 1;
                    }
                }
                value_end = cursor;
            }
            if bytes.get(cursor) != Some(&b')') {
                return Err(css_error());
            }
            index = cursor + 1;
            let value = decode_css_escapes(
                css.get(value_start..value_end)
                    .ok_or_else(css_error)?
                    .trim(),
            )?;
            urls.push(CssUrl {
                range: start..index,
                value,
            });
        } else if bytes[index] == b'\\' {
            index += 2;
        } else {
            index += 1;
        }
    }
    Ok(urls)
}

fn decode_css_escapes(value: &str) -> Result<String> {
    let mut characters = value.chars().peekable();
    let mut output = String::new();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        let next = characters.next().ok_or_else(css_error)?;
        if next.is_ascii_hexdigit() {
            let mut digits = String::from(next);
            while digits.len() < 6 && characters.peek().is_some_and(char::is_ascii_hexdigit) {
                digits.push(characters.next().ok_or_else(css_error)?);
            }
            let code = u32::from_str_radix(&digits, 16).map_err(|_| css_error())?;
            output.push(
                char::from_u32(code)
                    .filter(|character| *character != '\0')
                    .unwrap_or('\u{FFFD}'),
            );
            if characters
                .peek()
                .is_some_and(|character| character.is_ascii_whitespace())
            {
                characters.next();
            }
        } else if !matches!(next, '\n' | '\r' | '\u{000C}') {
            output.push(next);
        }
    }
    Ok(output)
}

fn strip_font_faces(css: &str) -> Result<String> {
    let bytes = css.as_bytes();
    let mut index = 0;
    let mut edits = Vec::new();
    while index < bytes.len() {
        if skip_css_literal(bytes, &mut index)? {
            continue;
        }
        if bytes
            .get(index..index + 10)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"@font-face"))
        {
            let start = index;
            index += 10;
            while index < bytes.len() {
                if bytes[index].is_ascii_whitespace() {
                    index += 1;
                } else if bytes.get(index..index + 2) == Some(b"/*") {
                    skip_css_literal(bytes, &mut index)?;
                } else {
                    break;
                }
            }
            if bytes.get(index) != Some(&b'{') {
                return Err(css_error());
            }
            let mut depth = 1_u32;
            index += 1;
            while index < bytes.len() && depth > 0 {
                if skip_css_literal(bytes, &mut index)? {
                    continue;
                }
                match bytes[index] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    b'\\' => {
                        index += 2;
                        continue;
                    }
                    _ => {}
                }
                index += 1;
            }
            if depth != 0 {
                return Err(css_error());
            }
            edits.push(Edit {
                range: start..index,
                replacement: String::new(),
            });
        } else if bytes[index] == b'\\' {
            index += 2;
        } else {
            index += 1;
        }
    }
    apply_edits(css, edits)
}

fn rewrite_css(css: &str, base: &str, options: &RewriteOptions<'_>) -> Result<String> {
    let css = if options.strip_fonts {
        strip_font_faces(css)?
    } else {
        css.to_owned()
    };
    let edits = css_urls(&css)?
        .into_iter()
        .filter(|url| {
            options.remove_images
                && (matches_resource(base, &url.value, options.images)
                    || is_embedded_image(&url.value))
        })
        .map(|url| Edit {
            range: url.range,
            replacement: "none".into(),
        })
        .collect();
    apply_edits(&css, edits)
}

fn is_embedded_image(value: &str) -> bool {
    value
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("data:image/")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use image::{ImageBuffer, Rgb};
    use tempfile::TempDir;

    use super::*;

    fn raster(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let image = ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([(x % 251) as u8, (y % 251) as u8, ((x + y) % 251) as u8])
        });
        encode_image(&DynamicImage::ImageRgb8(image), format, 95).unwrap()
    }

    fn fixture(body: &str, with_images: bool, with_font: bool) -> EpubDocument {
        let mut entries = BTreeMap::from([
            ("mimetype".into(), b"application/epub+zip".to_vec()),
            ("META-INF/container.xml".into(), br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OPS/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_vec()),
            ("OPS/chapter.xhtml".into(), format!(r#"<?xml version="1.0"?><!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd"><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Chapter</title><link rel="stylesheet" href="style.css"/></head><body><h1>Original title</h1><p>Original text remains.</p>{body}</body></html>"#).into_bytes()),
            ("OPS/style.css".into(), b"p{color:black;margin:1em} .illustration{background-image:url('cover.png')}".to_vec()),
        ]);
        let mut items = String::new();
        let mut cover = String::new();
        if with_images {
            entries.insert("OPS/cover.png".into(), raster(ImageFormat::Png, 1500, 900));
            entries.insert(
                "OPS/photo.jpg".into(),
                raster(ImageFormat::Jpeg, 1000, 1500),
            );
            items.push_str(r#"<item id="cover" href="cover.png" media-type="image/png" properties="cover-image"/><item id="photo" href="photo.jpg" media-type="image/jpeg"/>"#);
            cover.push_str(r#"<meta name="cover" content="cover"/>"#);
            cover.push_str(r##"<meta refines="#cover" property="display-seq">0</meta>"##);
        }
        if with_font {
            entries.insert("OPS/font.otf".into(), vec![0; 1200]);
            entries.insert("META-INF/encryption.xml".into(), br#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><EncryptedData xmlns="http://www.w3.org/2001/04/xmlenc#"><EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/><CipherData><CipherReference URI="OPS/font.otf"/></CipherData></EncryptedData></encryption>"#.to_vec());
            items.push_str(r#"<item id="font" href="font.otf" media-type="font/otf"/>"#);
            entries.insert("OPS/style.css".into(), br#"/* @font-face { ignored } */ @font-face { font-family:"A}B"; src:url('font.otf'); } p{color:black;margin:1em} .illustration{background-image:url('cover.png')}"#.to_vec());
        }
        entries.insert("OPS/book.opf".into(), format!(r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid"><metadata><dc:identifier id="uid">urn:test:optimizer</dc:identifier><dc:title>Optimization fixture</dc:title><dc:language>fr</dc:language><dc:creator>Example Author</dc:creator>{cover}<meta property="dcterms:modified">2026-10-09T00:00:00Z</meta></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="css" href="style.css" media-type="text/css"/>{items}</manifest><spine><itemref idref="chapter"/></spine></package>"#).into_bytes());
        EpubDocument::from_entries(entries, "Fixture").unwrap()
    }

    fn write_fixture(
        directory: &TempDir,
        body: &str,
        images: bool,
        font: bool,
    ) -> std::path::PathBuf {
        let path = directory.path().join("source.epub");
        fixture(body, images, font).write(&path, 6).unwrap();
        path
    }

    #[test]
    fn profiles_are_stable_and_lossless_preserves_every_resource_and_source() {
        assert_eq!(
            profiles().iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["lossless", "balanced", "xteink", "textOnly"]
        );
        assert!(profile("unknown").is_err());
        let directory = TempDir::new().unwrap();
        let source = write_fixture(&directory, "<p>Caption stays.</p>", true, true);
        let original = fs::read(&source).unwrap();
        let destination = directory.path().join("lossless.epub");
        let report = optimize(
            &source,
            &destination,
            &profile("lossless").unwrap(),
            "book",
            "variant",
        )
        .unwrap();
        let before = EpubDocument::open(&source).unwrap();
        let after = EpubDocument::open(&destination).unwrap();
        assert_eq!(before.entries, after.entries);
        assert_eq!(fs::read(&source).unwrap(), original);
        assert!(report.text_preserved);
        assert_eq!(
            (
                report.images_changed,
                report.images_removed,
                report.fonts_removed
            ),
            (0, 0, 0)
        );
        assert_eq!((report.chapters_before, report.chapters_after), (1, 1));
        assert_eq!(
            report.after_bytes,
            fs::metadata(&destination).unwrap().len()
        );
        assert!(
            optimize(
                &source,
                &destination,
                &profile("lossless").unwrap(),
                "book",
                "variant"
            )
            .is_err()
        );
        assert_eq!(fs::read(&source).unwrap(), original);
    }

    #[test]
    fn micro_profile_resizes_rasters_and_removes_obfuscated_fonts_without_orphans() {
        let directory = TempDir::new().unwrap();
        let source = write_fixture(
            &directory,
            r#"<figure><img src="cover.png" alt="Cover"/><figcaption>Caption stays.</figcaption></figure><style>@font-face{font-family:'Inline';src:url(font.otf)} p{padding:0}</style>"#,
            true,
            true,
        );
        let original = fs::read(&source).unwrap();
        let destination = directory.path().join("micro.epub");
        let report = optimize(
            &source,
            &destination,
            &profile("xteink").unwrap(),
            "book",
            "variant",
        )
        .unwrap();
        let after = EpubDocument::open(&destination).unwrap();
        assert_eq!(report.images_changed, 2);
        assert_eq!(report.fonts_removed, 1);
        assert!(!after.entries.contains_key("OPS/font.otf"));
        assert!(!after.entries.contains_key("META-INF/encryption.xml"));
        assert!(!after.manifest.iter().any(|item| item.id == "font"));
        for path in ["OPS/cover.png", "OPS/photo.jpg"] {
            let image = image::load_from_memory(&after.entries[path]).unwrap();
            assert!(image.width() <= 480 && image.height() <= 800);
            let pixel = image.to_rgb8().get_pixel(0, 0).0;
            assert_eq!(pixel[0], pixel[1]);
            assert_eq!(pixel[1], pixel[2]);
        }
        let css = source_text(&after.entries["OPS/style.css"]).unwrap();
        assert!(!css.contains("src:url('font.otf')"));
        assert!(css.contains("p{color:black;margin:1em}"));
        assert_eq!(after.cover_path.as_deref(), Some("OPS/cover.png"));
        assert_eq!(fs::read(&source).unwrap(), original);
        assert!(report.text_preserved);
    }

    #[test]
    fn balanced_retains_fonts_and_uses_its_explicit_image_bounds() {
        let directory = TempDir::new().unwrap();
        let source = write_fixture(&directory, "", true, true);
        let destination = directory.path().join("balanced.epub");
        let mut preset = profile("balanced").unwrap();
        preset.simplify_css = true;
        let report = optimize(&source, &destination, &preset, "book", "variant").unwrap();
        let after = EpubDocument::open(&destination).unwrap();
        assert!(after.entries.contains_key("OPS/font.otf"));
        let image = image::load_from_memory(&after.entries["OPS/cover.png"]).unwrap();
        assert!(image.width() <= 1200 && image.height() <= 1600);
        assert_eq!(report.fonts_removed, 0);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("CSS simplification"))
        );
        assert_eq!(
            after.entries["OPS/style.css"],
            EpubDocument::open(&source).unwrap().entries["OPS/style.css"]
        );
    }

    #[test]
    fn text_only_preserves_captions_vector_text_and_accessible_alternatives() {
        let directory = TempDir::new().unwrap();
        let source = write_fixture(
            &directory,
            r#"<figure><img src="cover.png" alt="Accessible cover"/><figcaption>Caption stays.</figcaption></figure><picture><source srcset="photo.jpg 1x"/><img src="photo.jpg" alt="Picture alternative"/></picture><svg xmlns="http://www.w3.org/2000/svg"><text>Vector label</text></svg><a href="cover.png">Image label stays.</a>"#,
            true,
            true,
        );
        let destination = directory.path().join("text.epub");
        let before = EpubDocument::open(&source).unwrap();
        let report = optimize(
            &source,
            &destination,
            &profile("textOnly").unwrap(),
            "book",
            "variant",
        )
        .unwrap();
        let after = EpubDocument::open(&destination).unwrap();
        assert_eq!(report.images_removed, 2);
        assert_eq!(report.fonts_removed, 1);
        assert!(
            !source_text(&after.entries["OPS/book.opf"])
                .unwrap()
                .contains("refines=\"#cover\"")
        );
        assert!(after.cover_path.is_none());
        assert!(
            !after
                .manifest
                .iter()
                .any(|item| item.media_type.starts_with("image/"))
        );
        assert_eq!(before.spine, after.spine);
        assert_eq!(
            before.text_fingerprint().unwrap(),
            after.text_fingerprint().unwrap()
        );
        let text = source_text(&after.entries["OPS/chapter.xhtml"]).unwrap();
        assert!(text.contains("aria-label=\"Accessible cover\""));
        assert!(text.contains("aria-label=\"Picture alternative\""));
        assert!(text.contains("Vector label"));
        assert!(text.contains("Caption stays."));
        assert!(text.contains("Image label stays."));
        assert!(!text.contains("href=\"cover.png\""));
        assert!(!text.contains("<picture"));
        assert!(
            !source_text(&after.entries["OPS/style.css"])
                .unwrap()
                .contains("url('cover.png')")
        );
    }

    #[test]
    fn text_only_removes_data_images_when_manifest_has_no_images() {
        let directory = TempDir::new().unwrap();
        let source = write_fixture(
            &directory,
            r#"<img src="data:image/png;base64,AAAA" alt="Embedded alternative"/><p style="background-image:url('data:image/png;base64,AAAA')">Paragraph stays.</p><svg xmlns="http://www.w3.org/2000/svg"><text>Vector text stays.</text></svg>"#,
            false,
            false,
        );
        let destination = directory.path().join("data-images.epub");
        optimize(
            &source,
            &destination,
            &profile("textOnly").unwrap(),
            "book",
            "variant",
        )
        .unwrap();
        let after = EpubDocument::open(&destination).unwrap();
        let text = source_text(&after.entries["OPS/chapter.xhtml"]).unwrap();
        assert!(!text.contains("data:image/"));
        assert!(!text.contains("<svg"));
        assert!(text.contains("Embedded alternative"));
        assert!(text.contains("Vector text stays."));
    }

    #[test]
    fn fonts_with_remaining_download_links_are_retained_and_reported() {
        let directory = TempDir::new().unwrap();
        let source = write_fixture(
            &directory,
            r#"<a href="font.otf">Font download</a>"#,
            false,
            true,
        );
        let destination = directory.path().join("font-retained.epub");
        let report = optimize(
            &source,
            &destination,
            &profile("xteink").unwrap(),
            "book",
            "variant",
        )
        .unwrap();
        let after = EpubDocument::open(&destination).unwrap();
        assert_eq!(report.fonts_removed, 0);
        assert!(after.entries.contains_key("OPS/font.otf"));
        assert!(after.entries.contains_key("META-INF/encryption.xml"));
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("fonts are retained"))
        );
    }

    #[test]
    fn decoder_rejects_oversized_raster_before_writing_any_variant() {
        let directory = TempDir::new().unwrap();
        let source = directory.path().join("bomb.epub");
        let mut document = fixture("", false, false);
        let mut png = raster(ImageFormat::Png, 1, 1);
        png[16..20].copy_from_slice(&16_384_u32.to_be_bytes());
        png[20..24].copy_from_slice(&16_384_u32.to_be_bytes());
        let mut crc = u32::MAX;
        for byte in &png[12..29] {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & 0_u32.wrapping_sub(crc & 1));
            }
        }
        png[29..33].copy_from_slice(&(!crc).to_be_bytes());
        document.entries.insert("OPS/bomb.png".into(), png);
        let opf = source_text(&document.entries["OPS/book.opf"])
            .unwrap()
            .replace(
                "</manifest>",
                r#"<item id="bomb" href="bomb.png" media-type="image/png"/></manifest>"#,
            );
        document
            .entries
            .insert("OPS/book.opf".into(), opf.into_bytes());
        document = EpubDocument::from_entries(document.entries, "Bomb fixture").unwrap();
        document.write(&source, 6).unwrap();
        let original = fs::read(&source).unwrap();
        let destination = directory.path().join("rejected.epub");
        assert!(matches!(
            optimize(
                &source,
                &destination,
                &profile("xteink").unwrap(),
                "book",
                "variant"
            ),
            Err(AppError::InvalidInput(_))
        ));
        assert!(!destination.exists());
        assert_eq!(fs::read(&source).unwrap(), original);
    }

    #[test]
    fn css_rewrite_understands_escaped_urls_and_braces_inside_strings() {
        let images = HashSet::from(["OPS/cover image.png".into()]);
        let fonts = HashSet::new();
        let options = RewriteOptions {
            images: &images,
            fonts: &fonts,
            remove_images: true,
            strip_fonts: true,
        };
        let rewritten = rewrite_css(r#"@font-face { font-family:'}'; src:url(font.otf) } /* url(keep.png) */ p{background:url(cover\20 image.png) white;content:'url(keep.png)'}"#, "OPS/style.css", &options).unwrap();
        assert!(!rewritten.contains("font-family"));
        assert!(rewritten.contains("background:none white"));
        assert!(rewritten.contains("/* url(keep.png) */"));
        assert!(rewritten.contains("content:'url(keep.png)'"));
        assert!(rewrite_css("@font-face {", "OPS/style.css", &options).is_err());
        assert!(
            rewrite_css(
                "p{background:url('unterminated)}",
                "OPS/style.css",
                &options
            )
            .is_err()
        );
    }
}
