//! Inactive EPUB reading, with managed storage and bounded per-section rendering.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    io::Cursor,
    sync::{Arc, Mutex},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use image::{ImageFormat, ImageReader, Limits};
use url::Url;

use crate::{
    AppError, Book, BookFormat, BookRepository, EpubDocument, FileVariant, ReaderManifest,
    ReaderResource, ReaderSection, ReaderSectionInfo, ReaderTocItem, Result, Storage, TocEntry,
    epub::{escape_xml, resolve_href, text_for_inspection},
};

const MAX_CHAPTER_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_EMBEDDED_URL_BYTES: usize = 12 * 1024 * 1024;
const MAX_RENDERED_BYTES: usize = 16 * 1024 * 1024;
const MAX_SECTIONS: usize = 10_000;
const MAX_LOCATION_BYTES: usize = 1024;
const CSP: &str = "default-src 'none'; script-src 'none'; connect-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; navigate-to 'none'";
const STYLE: &str = "html{color-scheme:light}body{margin:0;padding:24px;color:#17202a;background:#faf9f5;font-family:Georgia,serif;font-size:18px;line-height:1.7;overflow-wrap:anywhere}h1,h2,h3,h4,h5,h6{line-height:1.3}img{max-width:100%;height:auto}pre{white-space:pre-wrap}a{color:#315b9c}table{max-width:100%;border-collapse:collapse}td,th{padding:4px;border:1px solid #ddd}blockquote{margin-inline:16px}hr{border:0;border-top:1px solid #ddd}";

#[derive(Debug, Clone)]
pub struct ReaderService {
    repository: BookRepository,
    storage: Storage,
    gate: Arc<Mutex<()>>,
}

impl ReaderService {
    pub fn new(repository: BookRepository, storage: Storage) -> Self {
        Self {
            repository,
            storage,
            gate: Arc::new(Mutex::new(())),
        }
    }

    pub fn open(&self, book_id: &str) -> Result<ReaderManifest> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| invalid("Reader operation is unavailable"))?;
        let (book, document) = self.load(book_id)?;
        let toc = reader_toc(&document.toc, &document.spine, 0)?;
        let sections = document
            .spine
            .iter()
            .enumerate()
            .map(|(index, path)| ReaderSectionInfo {
                index: index as u64,
                title: toc_title(&toc, index as u64)
                    .unwrap_or_else(|| format!("{} {}", book.title, index + 1)),
                size_bytes: document.entries[path].len() as u64,
            })
            .collect();
        let saved_location = self
            .repository
            .reader_location(book_id)?
            .filter(|location| validate_location(location, document.spine.len()).is_ok());
        Ok(ReaderManifest {
            book_id: book.id,
            title: book.title,
            authors: book.authors,
            sections,
            toc,
            saved_location,
            saved_progress: book.reading_progress,
        })
    }

    pub fn section(&self, book_id: &str, index: u64) -> Result<ReaderSection> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| invalid("Reader operation is unavailable"))?;
        let (book, mut document) = self.load(book_id)?;
        let index_usize =
            usize::try_from(index).map_err(|_| invalid("Reader section index is invalid"))?;
        let path = document
            .spine
            .get(index_usize)
            .cloned()
            .ok_or_else(|| invalid("Reader section is outside the spine"))?;
        let bytes = &document.entries[&path];
        if bytes.len() > MAX_CHAPTER_BYTES {
            return Err(AppError::Unsupported(
                "The chapter exceeds the reader rendering limit".into(),
            ));
        }
        let source = std::str::from_utf8(bytes)
            .map_err(|_| {
                AppError::Unsupported("Reading requires UTF-8 XHTML or HTML chapters".into())
            })?
            .to_owned();
        // This also rejects internal DTD/entity declarations before HTML5 parsing.
        let (plain_text, fallback) = text_for_inspection(&source)?;
        let mut initial_warnings = Vec::new();
        if fallback {
            initial_warnings.push("readerHtmlNormalization".into());
        }
        let source = if document
            .manifest
            .iter()
            .any(|item| item.href == path && item.media_type == "image/svg+xml")
        {
            initial_warnings.push("readerSvgTextOnly".into());
            format!("<pre>{}</pre>", escape_xml(&plain_text))
        } else {
            escaped_xml_text(&source)
        };
        document.entries.remove(&path);
        let document = Arc::new(document);
        let state = Arc::new(Mutex::new(RenderState {
            warnings: initial_warnings,
            ..RenderState::default()
        }));
        let filter_state = state.clone();
        let filter_document = document.clone();
        let filter_path = path.clone();
        let inactive = [
            "script", "style", "iframe", "frame", "frameset", "form", "input", "select", "button",
            "textarea", "object", "embed", "audio", "video", "svg", "math", "meta", "base", "link",
            "title",
        ];
        let sanitized = ammonia::Builder::default()
            .rm_tags(&inactive)
            .add_clean_content_tags(&inactive)
            .generic_attributes(HashSet::from(["id", "lang", "dir", "title"]))
            .tag_attributes(HashMap::from([
                ("a", HashSet::from(["href"])),
                ("img", HashSet::from(["src", "alt"])),
                ("ol", HashSet::from(["start", "type"])),
                ("li", HashSet::from(["value"])),
                ("td", HashSet::from(["colspan", "rowspan"])),
                ("th", HashSet::from(["colspan", "rowspan"])),
            ]))
            .url_schemes(HashSet::from(["https", "http", "data"]))
            .link_rel(Some("noopener noreferrer"))
            .attribute_filter(move |element, attribute, value| {
                if element == "img" && attribute == "src" {
                    let mut state = filter_state.lock().ok()?;
                    return image_url(&filter_document, &filter_path, value, &mut state)
                        .map(Cow::Owned);
                }
                if element == "a" && attribute == "href" {
                    return safe_link(&filter_path, value).map(Cow::Owned);
                }
                if attribute == "id" && !valid_fragment(value) {
                    return None;
                }
                if matches!(attribute, "colspan" | "rowspan")
                    && value
                        .parse::<u32>()
                        .ok()
                        .is_none_or(|span| span == 0 || span > 100)
                {
                    return None;
                }
                Some(Cow::Borrowed(value))
            })
            .clean(&source)
            .to_string();
        // Only external anchors get a popup target. The desktop iframe omits
        // allow-popups/top-navigation, while local footnotes can still scroll.
        let external_anchor = regex::Regex::new(r#"<a(\s[^>]*\bhref=\"https?://[^\">]*\"[^>]*)>"#)
            .map_err(|_| invalid("Reader link policy is invalid"))?;
        let sanitized = external_anchor.replace_all(&sanitized, "<a$1 target=\"_blank\">");
        let html = format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"{}\"><style>{STYLE}</style></head><body>{sanitized}</body></html>",
            escape_xml(CSP)
        );
        if html.len() > MAX_RENDERED_BYTES {
            return Err(AppError::Unsupported(
                "Rendered reader content exceeds the safety limit".into(),
            ));
        }
        let mut state = state
            .lock()
            .map_err(|_| invalid("Reader resource state is unavailable"))?;
        if source.to_ascii_lowercase().contains("<svg") {
            state.warnings.push("readerSvgOmitted".into());
        }
        state.warnings.sort();
        state.warnings.dedup();
        Ok(ReaderSection {
            book_id: book.id,
            section_index: index,
            html,
            resources: std::mem::take(&mut state.resources),
            warnings: std::mem::take(&mut state.warnings),
        })
    }

    pub fn save_progress(&self, book_id: &str, location: &str, progress: f64) -> Result<()> {
        if !progress.is_finite() || !(0.0..=1.0).contains(&progress) {
            return Err(invalid("Reading progress must be between zero and one"));
        }
        let _guard = self
            .gate
            .lock()
            .map_err(|_| invalid("Reader operation is unavailable"))?;
        let (_, document) = self.load(book_id)?;
        validate_location(location, document.spine.len())?;
        self.repository.save_progress(book_id, location, progress)
    }

    fn load(&self, book_id: &str) -> Result<(Book, EpubDocument)> {
        if book_id.is_empty()
            || book_id.len() > 128
            || !book_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(invalid("Reader book ID is invalid"));
        }
        let book = self.repository.get(book_id, &[])?;
        let mut files: Vec<_> = self
            .repository
            .files(book_id)?
            .into_iter()
            .filter(|stored| stored.file.format == BookFormat::Epub)
            .collect();
        files.sort_by_key(|stored| match stored.file.variant {
            FileVariant::Normalized => 0,
            FileVariant::Converted => 1,
            FileVariant::Original => 2,
            FileVariant::Optimized => 3,
        });
        let selected = files.first().ok_or_else(|| {
            AppError::Unsupported(
                "The integrated reader requires an EPUB original or converted variant".into(),
            )
        })?;
        let bytes = self.storage.read(&selected.relative_path)?;
        let document = EpubDocument::from_bytes(&bytes, &book.title)?;
        if document.spine.len() > MAX_SECTIONS {
            return Err(AppError::Unsupported(
                "The reader chapter count exceeds the safety limit".into(),
            ));
        }
        Ok((book, document))
    }
}

#[derive(Default)]
struct RenderState {
    resources: Vec<ReaderResource>,
    warnings: Vec<String>,
    embedded: HashMap<String, String>,
    image_bytes: usize,
    embedded_url_bytes: usize,
}

fn invalid(message: &str) -> AppError {
    AppError::InvalidInput(message.into())
}

fn escaped_xml_text(source: &str) -> String {
    let Ok(document) = crate::epub::parse_markup(source) else {
        return source.to_owned();
    };
    let mut replacements: Vec<_> = document
        .descendants()
        .filter(|node| node.is_text())
        .map(|node| (node.range(), escape_xml(node.text().unwrap_or_default())))
        .collect();
    replacements.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut output = source.to_owned();
    for (range, replacement) in replacements {
        output.replace_range(range, &replacement);
    }
    output
}

fn valid_fragment(fragment: &str) -> bool {
    !fragment.is_empty()
        && fragment.len() <= 256
        && !fragment.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '<' | '>' | '"' | '\'' | '#')
        })
}

fn validate_location(location: &str, section_count: usize) -> Result<()> {
    if location.len() > MAX_LOCATION_BYTES {
        return Err(invalid("Reader location exceeds the safety limit"));
    }
    let content = location
        .strip_prefix("section:")
        .ok_or_else(|| invalid("Reader location is invalid"))?;
    let (index, fragment) = content
        .split_once('#')
        .map_or((content, None), |(index, fragment)| (index, Some(fragment)));
    if index.is_empty()
        || !index.bytes().all(|byte| byte.is_ascii_digit())
        || index
            .parse::<usize>()
            .ok()
            .is_none_or(|index| index >= section_count)
        || fragment.is_some_and(|fragment| !valid_fragment(fragment))
    {
        return Err(invalid("Reader location is outside the book"));
    }
    Ok(())
}

fn reader_toc(entries: &[TocEntry], spine: &[String], depth: usize) -> Result<Vec<ReaderTocItem>> {
    if depth > 64 {
        return Err(invalid(
            "Reader navigation nesting exceeds the safety limit",
        ));
    }
    let mut output = Vec::new();
    for entry in entries {
        let (path, fragment) = entry
            .href
            .split_once('#')
            .map_or((entry.href.as_str(), None), |(path, fragment)| {
                (path, Some(fragment))
            });
        let children = reader_toc(&entry.children, spine, depth + 1)?;
        if let Some(index) = spine.iter().position(|candidate| candidate == path) {
            output.push(ReaderTocItem {
                label: entry.label.chars().take(512).collect(),
                section_index: index as u64,
                fragment: fragment
                    .filter(|value| valid_fragment(value))
                    .map(str::to_owned),
                children,
            });
        } else {
            output.extend(children);
        }
    }
    Ok(output)
}

fn toc_title(toc: &[ReaderTocItem], index: u64) -> Option<String> {
    for entry in toc {
        if entry.section_index == index && !entry.label.trim().is_empty() {
            return Some(entry.label.clone());
        }
        if let Some(title) = toc_title(&entry.children, index) {
            return Some(title);
        }
    }
    None
}

fn safe_link(base_path: &str, value: &str) -> Option<String> {
    if let Some(fragment) = value.strip_prefix('#') {
        return valid_fragment(fragment).then(|| value.to_owned());
    }
    if let Ok(url) = Url::parse(value) {
        return (matches!(url.scheme(), "https" | "http")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none())
        .then(|| url.to_string());
    }
    let path = resolve_href(base_path, value).ok()?;
    let fragment = value.split_once('#')?.1;
    (path == base_path && valid_fragment(fragment)).then(|| format!("#{fragment}"))
}

fn image_url(
    document: &EpubDocument,
    base_path: &str,
    source: &str,
    state: &mut RenderState,
) -> Option<String> {
    let key = if source.starts_with("data:") {
        source.to_owned()
    } else {
        match resolve_href(base_path, source) {
            Ok(path) => path,
            Err(_) => {
                state.warnings.push("readerExternalImageOmitted".into());
                return None;
            }
        }
    };
    if let Some(url) = state.embedded.get(&key).cloned() {
        return budget_url(url, state);
    }
    let (mime, bytes) = if let Some(uri) = source.strip_prefix("data:") {
        let (header, content) = uri.split_once(',')?;
        let mime = match header {
            "image/png;base64" => "image/png",
            "image/jpeg;base64" => "image/jpeg",
            _ => {
                state.warnings.push("readerUnsupportedImageOmitted".into());
                return None;
            }
        };
        if content.len() > MAX_EMBEDDED_URL_BYTES {
            state.warnings.push("readerImageLimit".into());
            return None;
        }
        (mime.to_owned(), STANDARD.decode(content).ok()?)
    } else {
        let item = document.manifest.iter().find(|item| item.href == key)?;
        if !matches!(item.media_type.as_str(), "image/png" | "image/jpeg") {
            state.warnings.push(
                if item.media_type == "image/svg+xml" {
                    "readerSvgOmitted"
                } else {
                    "readerUnsupportedImageOmitted"
                }
                .into(),
            );
            return None;
        }
        let bytes = document.entries.get(&key)?;
        if bytes.len() > MAX_IMAGE_BYTES {
            state.warnings.push("readerImageLimit".into());
            return None;
        }
        (item.media_type.clone(), bytes.clone())
    };
    if bytes.len() > MAX_IMAGE_BYTES.saturating_sub(state.image_bytes) {
        state.warnings.push("readerImageLimit".into());
        return None;
    }
    if !valid_raster(&bytes, &mime) {
        state.warnings.push("readerInvalidImageOmitted".into());
        return None;
    }
    let url = format!("data:{mime};base64,{}", STANDARD.encode(&bytes));
    state.image_bytes += bytes.len();
    state.resources.push(ReaderResource {
        id: format!("reader-image-{}", state.resources.len()),
        url: url.clone(),
        media_type: mime,
    });
    state.embedded.insert(key, url.clone());
    budget_url(url, state)
}

fn budget_url(url: String, state: &mut RenderState) -> Option<String> {
    if url.len() > MAX_EMBEDDED_URL_BYTES.saturating_sub(state.embedded_url_bytes) {
        state.warnings.push("readerImageLimit".into());
        return None;
    }
    state.embedded_url_bytes += url.len();
    Some(url)
}

fn valid_raster(bytes: &[u8], mime: &str) -> bool {
    let Ok(mut reader) = ImageReader::new(Cursor::new(bytes)).with_guessed_format() else {
        return false;
    };
    let expected = if mime == "image/png" {
        ImageFormat::Png
    } else {
        ImageFormat::Jpeg
    };
    if reader.format() != Some(expected) {
        return false;
    }
    let mut limits = Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    reader.into_dimensions().is_ok_and(|(width, height)| {
        u64::from(width)
            .checked_mul(u64::from(height))
            .is_some_and(|pixels| pixels > 0 && pixels <= 16_000_000)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BookFile, BookMetadata, Database, ReadStatus, StoredFile};
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use std::{collections::BTreeMap, fs};
    use tempfile::TempDir;

    fn png() -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_pixel(3, 2, Rgb([150, 90, 60])))
            .write_to(&mut output, ImageFormat::Png)
            .unwrap();
        output.into_inner()
    }

    fn entries(chapter: &str, image: Vec<u8>) -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            ("mimetype".into(), b"application/epub+zip".to_vec()),
            ("META-INF/container.xml".into(), br#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_vec()),
            ("OPS/content.opf".into(), br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid"><metadata><dc:identifier id="uid">urn:uuid:reader-test</dc:identifier><dc:title>Lecteur</dc:title><dc:language>fr</dc:language></metadata><manifest><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="image" href="image.png" media-type="image/png"/><item id="svg" href="image.svg" media-type="image/svg+xml"/></manifest><spine><itemref idref="a"/><itemref idref="b"/></spine></package>"#.to_vec()),
            ("OPS/a.xhtml".into(), chapter.as_bytes().to_vec()),
            ("OPS/b.xhtml".into(), b"<html><body><p id='end'>Dernier chapitre.</p></body></html>".to_vec()),
            ("OPS/nav.xhtml".into(), r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="a.xhtml#intro">Première partie</a><ol><li><a href="b.xhtml#end">Deuxième partie</a></li></ol></li></ol></nav></body></html>"#.as_bytes().to_vec()),
            ("OPS/image.png".into(), image),
            ("OPS/image.svg".into(), b"<svg xmlns='http://www.w3.org/2000/svg'><script>bad()</script><text>Vector</text></svg>".to_vec()),
        ])
    }

    fn register(
        reader: &ReaderService,
        directory: &TempDir,
        source_entries: BTreeMap<String, Vec<u8>>,
        variant: FileVariant,
        id: &str,
    ) {
        let source = directory.path().join(format!("{id}.epub"));
        EpubDocument::from_entries(source_entries, "Lecteur")
            .unwrap()
            .write(&source, 9)
            .unwrap();
        let artifact = reader
            .storage
            .publish_file(&format!("books/reader/{id}.epub"), &source)
            .unwrap();
        let stored = StoredFile {
            file: BookFile {
                id: id.into(),
                book_id: "book".into(),
                format: BookFormat::Epub,
                variant,
                profile: None,
                size_bytes: artifact.size_bytes,
                sha256: artifact.sha256,
                created_at: "2026-10-09T00:00:00Z".into(),
            },
            relative_path: artifact.relative_path,
        };
        if reader.repository.get("book", &[]).is_ok() {
            reader.repository.add_file(stored).unwrap();
        } else {
            reader
                .repository
                .insert(
                    "book",
                    BookMetadata {
                        title: "Lecteur".into(),
                        authors: vec!["Auteur".into()],
                        language: "fr".into(),
                        ..BookMetadata::default()
                    },
                    &[stored],
                    None,
                )
                .unwrap();
        }
    }

    fn fixture(chapter: &str) -> (TempDir, ReaderService) {
        let directory = TempDir::new().unwrap();
        let repository =
            BookRepository::new(Database::new(&directory.path().join("database")).unwrap());
        let storage = Storage::new(&directory.path().join("library")).unwrap();
        let reader = ReaderService::new(repository, storage);
        register(
            &reader,
            &directory,
            entries(chapter, png()),
            FileVariant::Original,
            "original",
        );
        (directory, reader)
    }

    #[test]
    fn active_content_external_resources_and_traversal_are_blocked() {
        let (_directory, reader) = fixture(
            r##"<html><head><meta http-equiv="refresh" content="0;url=https://invalid.example"/><style>p{background:url(https://invalid.example)}</style></head><body><h1 id="intro">Bonjour</h1><p>Café &amp; texte.</p><script>private_script()</script><iframe src="https://invalid.example">iframe_secret</iframe><form action="https://invalid.example"><input name="secret"/></form><img src="image.png" alt="image locale" onerror="bad()"/><img src="../../../etc/passwd"/><img src="https://invalid.example/remote.png"/><img src="image.svg"/><svg><script>bad()</script></svg><a href="javascript:bad()">dangereux</a><a href="https://example.org/page">source</a><a href="#intro">note</a><p style="color:red" onclick="bad()">Fin.</p></body></html>"##,
        );
        let section = reader.section("book", 0).unwrap();
        for forbidden in [
            "<script",
            "onerror",
            "onclick",
            "<iframe",
            "iframe_secret",
            "<form",
            "<input",
            "javascript:",
            "url(",
            "../../../",
            "invalid.example",
            "http-equiv=\"refresh\"",
            "style=\"",
        ] {
            assert!(!section.html.contains(forbidden), "{forbidden}");
        }
        assert!(section.html.contains("Café &amp; texte."));
        assert!(section.html.contains("href=\"https://example.org/page\""));
        assert!(section.html.contains("target=\"_blank\""));
        assert!(section.html.contains("href=\"#intro\""));
        assert!(
            section.html.contains("default-src &#39;none&#39;")
                || section.html.contains("default-src &apos;none&apos;")
        );
        assert_eq!(section.resources.len(), 1);
        assert!(
            section.resources[0]
                .url
                .starts_with("data:image/png;base64,")
        );
        assert!(section.warnings.contains(&"readerSvgOmitted".into()));
        assert!(
            section
                .warnings
                .contains(&"readerExternalImageOmitted".into())
        );
    }

    #[test]
    fn nested_navigation_and_progress_are_bounded_persistent_and_idempotent() {
        let (_directory, reader) =
            fixture("<html><body><p id='intro'>Premier chapitre.</p></body></html>");
        let manifest = reader.open("book").unwrap();
        assert_eq!(manifest.sections.len(), 2);
        assert_eq!(manifest.toc[0].section_index, 0);
        assert_eq!(manifest.toc[0].fragment, Some("intro".into()));
        assert_eq!(manifest.toc[0].children[0].section_index, 1);
        reader.save_progress("book", "section:1#end", 0.5).unwrap();
        let updated = reader.open("book").unwrap();
        assert_eq!(updated.saved_location, Some("section:1#end".into()));
        assert_eq!(updated.saved_progress, 0.5);
        let revision = reader.repository.get("book", &[]).unwrap().revision;
        reader.save_progress("book", "section:1#end", 0.5).unwrap();
        assert_eq!(
            reader.repository.get("book", &[]).unwrap().revision,
            revision
        );
        for location in [
            "section:2",
            "section:-1",
            "file:///etc/passwd",
            "section:0#bad\n",
            "section:0#",
        ] {
            assert!(reader.save_progress("book", location, 0.5).is_err());
        }
        for progress in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            assert!(reader.save_progress("book", "section:0", progress).is_err());
        }
        assert!(reader.section("book", u64::MAX).is_err());
        reader.save_progress("book", "section:1", 1.0).unwrap();
        assert_eq!(
            reader.repository.get("book", &[]).unwrap().read_status,
            ReadStatus::Finished
        );
    }

    #[test]
    fn normalized_variant_is_preferred_and_other_formats_require_an_epub_variant() {
        let (directory, reader) = fixture("<html><body><p>Original.</p></body></html>");
        register(
            &reader,
            &directory,
            entries("<html><body><p>Normalisé.</p></body></html>", png()),
            FileVariant::Normalized,
            "normalized",
        );
        assert!(
            reader
                .section("book", 0)
                .unwrap()
                .html
                .contains("Normalisé.")
        );
        reader
            .repository
            .insert(
                "txt-book",
                BookMetadata {
                    title: "Un texte".into(),
                    ..BookMetadata::default()
                },
                &[],
                None,
            )
            .unwrap();
        assert!(matches!(
            reader.open("txt-book"),
            Err(AppError::Unsupported(_))
        ));
        assert!(matches!(
            reader.open("../book"),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[test]
    fn html5_fallback_is_inactive_and_internal_entity_definitions_are_refused() {
        let (_directory, reader) = fixture(
            "<!doctype html><html><body><p>Texte&nbsp;français<br>Suite</p><img src='image.png' onerror='bad()'></body></html>",
        );
        let section = reader.section("book", 0).unwrap();
        assert!(section.html.contains("Suite"));
        assert!(!section.html.contains("onerror"));
        assert!(section.warnings.contains(&"readerHtmlNormalization".into()));
        let (_directory, reader) = fixture(
            "<!DOCTYPE html [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><html><body>&x;</body></html>",
        );
        assert!(reader.section("book", 0).is_err());
    }

    #[test]
    fn repeated_images_cannot_multiply_data_urls_beyond_the_render_budget() {
        let directory = TempDir::new().unwrap();
        let reader = ReaderService::new(
            BookRepository::new(Database::new(&directory.path().join("database")).unwrap()),
            Storage::new(&directory.path().join("library")).unwrap(),
        );
        let mut image = png();
        let mut seed = 0xabcdef0123456789_u64;
        for _ in 0..1024 * 1024 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            image.push(seed as u8);
        }
        register(
            &reader,
            &directory,
            entries(
                &format!(
                    "<html><body><p>Visible.</p>{}</body></html>",
                    "<img src='image.png'/>".repeat(20)
                ),
                image,
            ),
            FileVariant::Original,
            "original",
        );
        let section = reader.section("book", 0).unwrap();
        assert_eq!(section.resources.len(), 1);
        assert!(section.html.len() < MAX_RENDERED_BYTES);
        assert!(section.html.contains("Visible."));
        assert!(section.warnings.contains(&"readerImageLimit".into()));
    }

    #[test]
    fn xml_literal_text_is_preserved_and_storage_symlinks_are_refused() {
        let (directory, reader) = fixture(
            "<html><body><pre><![CDATA[literal <!ENTITY example> & symbols]]></pre><p>co<em>de</em>base</p></body></html>",
        );
        let section = reader.section("book", 0).unwrap();
        assert!(
            section
                .html
                .contains("literal &lt;!ENTITY example&gt; &amp; symbols")
        );
        assert!(section.html.contains("co<em>de</em>base"));
        let file = reader.repository.files("book").unwrap().remove(0);
        let path = reader.storage.resolve(&file.relative_path).unwrap();
        fs::rename(&path, directory.path().join("kept.epub")).unwrap();
        std::os::unix::fs::symlink(directory.path().join("kept.epub"), &path).unwrap();
        assert!(reader.section("book", 0).is_err());
    }

    #[test]
    fn svg_spine_is_read_as_inactive_text_with_a_warning() {
        let directory = TempDir::new().unwrap();
        let reader = ReaderService::new(
            BookRepository::new(Database::new(&directory.path().join("database")).unwrap()),
            Storage::new(&directory.path().join("library")).unwrap(),
        );
        let mut source = entries(
            "<svg xmlns='http://www.w3.org/2000/svg'><script>bad_secret()</script><text>Texte vectoriel lisible.</text></svg>",
            png(),
        );
        let opf = String::from_utf8(source.remove("OPS/content.opf").unwrap())
            .unwrap()
            .replace(
                "id=\"a\" href=\"a.xhtml\" media-type=\"application/xhtml+xml\"",
                "id=\"a\" href=\"a.xhtml\" media-type=\"image/svg+xml\"",
            );
        source.insert("OPS/content.opf".into(), opf.into_bytes());
        register(
            &reader,
            &directory,
            source,
            FileVariant::Original,
            "svg-source",
        );
        let section = reader.section("book", 0).unwrap();
        assert!(section.html.contains("Texte vectoriel lisible."));
        assert!(!section.html.contains("bad_secret"));
        assert!(!section.html.contains("<svg"));
        assert!(section.warnings.contains(&"readerSvgTextOnly".into()));
    }
}
