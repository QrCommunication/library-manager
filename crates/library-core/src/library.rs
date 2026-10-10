//! Library orchestration, immutable originals and auditable active variants.

use std::{
    collections::HashSet,
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use image::{ImageReader, Limits, codecs::jpeg::JpegEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::{
    AppError, Book, BookFile, BookFormat, BookMetadata, BookPage, BookPatch, BookQuery,
    BookRepository, ConversionReport, Converter, EpubDocument, FileVariant, LibraryFacets,
    MetadataStatus, Operation, OptimizationReport, Result, Storage, StoredArtifact, StoredFile,
    epub::inspect_for_import,
    optimizer,
    secure_fs::{AccessPolicy, SecureDir},
};

const MAX_COVER_BYTES: usize = 8 * 1024 * 1024;
const MAX_THUMBNAIL_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub book: Book,
    pub duplicate: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LibraryService {
    storage: Storage,
    repository: BookRepository,
    converter: Converter,
    gate: Arc<Mutex<()>>,
    cancellation: Option<Arc<AtomicBool>>,
}

#[derive(Default)]
struct UpdateContext<'a> {
    enrichment: Option<(MetadataStatus, f64)>,
    original_sha256: Option<&'a str>,
    inspected_file: Option<(&'a str, &'a str)>,
    review_job_id: Option<&'a str>,
}

impl LibraryService {
    pub fn new(storage: Storage, repository: BookRepository, converter: Converter) -> Self {
        Self {
            storage,
            repository,
            converter,
            gate: Arc::new(Mutex::new(())),
            cancellation: None,
        }
    }

    /// Attach the job's cancellation signal to this clone only.
    pub fn with_cancellation(mut self, cancellation: Arc<AtomicBool>) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    pub(crate) fn check_cancelled(&self) -> Result<()> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(|signal| signal.load(Ordering::Acquire))
        {
            return Err(AppError::Cancelled);
        }
        Ok(())
    }

    pub fn import(&self, source: &Path) -> Result<ImportOutcome> {
        self.import_source(source, None)
    }

    /// A device import must copy exactly the bytes identified by its inventory.
    pub fn import_expected(&self, source: &Path, expected_sha256: &str) -> Result<ImportOutcome> {
        if expected_sha256.len() != 64
            || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid(
                "Expected import hash must contain 64 hexadecimal characters",
            ));
        }
        self.import_source(source, Some(expected_sha256))
    }

    fn import_source(&self, source: &Path, expected_sha256: Option<&str>) -> Result<ImportOutcome> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| invalid("Library operation is unavailable"))?;
        self.check_cancelled()?;
        let format = detect_format(source)?;
        let original = self.storage.import_original(source, format)?;
        if expected_sha256.is_some_and(|expected| !original.sha256.eq_ignore_ascii_case(expected)) {
            return Err(AppError::Conflict(
                "Source file changed since device inventory".into(),
            ));
        }
        self.check_cancelled()?;
        if let Some(existing) = self.repository.find_original_hash(&original.sha256)? {
            return Ok(ImportOutcome {
                book: self.decorate(self.repository.get(&existing.file.book_id, &[])?),
                duplicate: true,
                warnings: Vec::new(),
            });
        }
        let book_id = Uuid::new_v4().to_string();
        let work = WorkDirectory::new()?;
        let mut warnings = Vec::new();
        let mut metadata = BookMetadata {
            title: source
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Untitled")
                .into(),
            ..BookMetadata::default()
        };
        let mut transformable = false;
        let mut cover = None;
        let bytes = if format == BookFormat::Epub {
            match self.storage.read(&original.relative_path) {
                Ok(bytes) => Some(bytes),
                Err(_) => {
                    warnings.push("importEpubReadLimit".into());
                    None
                }
            }
        } else {
            None
        };
        if let Some(bytes) = &bytes {
            self.check_cancelled()?;
            let inspection_path = work.path.join(
                source
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("input.epub")),
            );
            write_stage(&inspection_path, bytes)?;
            match inspect_for_import(&inspection_path) {
                Ok(inspection) => {
                    metadata = inspection.metadata;
                    transformable = inspection.transformable;
                    warnings.extend(inspection.warnings);
                    if let Some(path) = inspection.cover_path {
                        match self.thumbnail(bytes, &path) {
                            Ok(path) => cover = Some(path),
                            Err(_) => warnings.push("importCoverUnavailable".into()),
                        }
                    }
                }
                Err(_) => warnings.push("importEpubInspectionFailed".into()),
            }
        }
        metadata = normalize_metadata(metadata, false, &mut warnings)?;
        self.check_cancelled()?;
        let mut files = vec![stored_file(
            &book_id,
            FileVariant::Original,
            format,
            None,
            original,
        )];
        if transformable && let Some(bytes) = &bytes {
            match self.normalized_variant(&book_id, &metadata, bytes, &work) {
                Ok(Some(file)) => {
                    if file.file.sha256 != files[0].file.sha256 {
                        files.push(file);
                    }
                }
                Ok(None) => {}
                Err(_) => warnings.push("importNormalizationUnavailable".into()),
            }
        }
        self.check_cancelled()?;
        let mut book =
            match self
                .repository
                .insert_audited(&book_id, metadata, &files, cover.as_deref())
            {
                Ok(book) => book,
                Err(error) => {
                    // A concurrent process can import the same original after the
                    // first lookup. Preserve its canonical record instead.
                    if let Some(existing) =
                        self.repository.find_original_hash(&files[0].file.sha256)?
                    {
                        return Ok(ImportOutcome {
                            book: self.decorate(self.repository.get(&existing.file.book_id, &[])?),
                            duplicate: true,
                            warnings,
                        });
                    }
                    return Err(error);
                }
            };
        if !warnings.is_empty() {
            self.check_cancelled()?;
            self.repository
                .set_metadata_status(&book.id, MetadataStatus::NeedsReview, None)?;
            book = self.repository.get(&book.id, &[])?;
        }
        Ok(ImportOutcome {
            book: self.decorate(book),
            duplicate: false,
            warnings,
        })
    }

    pub fn list(&self, query: &BookQuery, connected: &[String]) -> Result<BookPage> {
        let mut page = self.repository.list(query, connected)?;
        page.items = page
            .items
            .into_iter()
            .map(|book| self.decorate(book))
            .collect();
        Ok(page)
    }
    pub fn get(&self, id: &str, connected: &[String]) -> Result<Book> {
        Ok(self.decorate(self.repository.get(id, connected)?))
    }
    pub fn facets(&self, connected: &[String]) -> Result<LibraryFacets> {
        self.repository.facets(connected)
    }
    pub fn files(&self, id: &str) -> Result<Vec<BookFile>> {
        Ok(self
            .repository
            .files(id)?
            .into_iter()
            .map(|stored| stored.file)
            .collect())
    }

    pub fn update(&self, id: &str, patch: &BookPatch, expected_revision: u64) -> Result<Book> {
        self.update_internal(id, patch, expected_revision, UpdateContext::default())
    }

    /// Resolve an explicit human review even when its metadata values are already unchanged.
    pub fn review_book(
        &self,
        id: &str,
        job_id: &str,
        patch: &BookPatch,
        expected_revision: u64,
    ) -> Result<Book> {
        self.update_internal(
            id,
            patch,
            expected_revision,
            UpdateContext {
                review_job_id: Some(job_id),
                ..UpdateContext::default()
            },
        )
    }

    /// Apply a reviewed patch only while the inspected immutable original still matches.
    pub fn update_inspected(
        &self,
        id: &str,
        patch: &BookPatch,
        expected_revision: u64,
        original_sha256: &str,
    ) -> Result<Book> {
        if original_sha256.len() != 64
            || !original_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid(
                "Inspected original hash must contain 64 hexadecimal characters",
            ));
        }
        self.update_internal(
            id,
            patch,
            expected_revision,
            UpdateContext {
                original_sha256: Some(original_sha256),
                ..UpdateContext::default()
            },
        )
    }

    /// Bind a reviewed EPUB inspection and its immutable source separately before changing metadata.
    pub fn update_from_inspection(
        &self,
        id: &str,
        patch: &BookPatch,
        expected_revision: u64,
        original_sha256: &str,
        inspected_file_id: &str,
        inspected_sha256: &str,
    ) -> Result<Book> {
        for hash in [original_sha256, inspected_sha256] {
            if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(invalid(
                    "Inspection hashes must contain 64 hexadecimal characters",
                ));
            }
        }
        self.update_internal(
            id,
            patch,
            expected_revision,
            UpdateContext {
                original_sha256: Some(original_sha256),
                inspected_file: Some((inspected_file_id, inspected_sha256)),
                ..UpdateContext::default()
            },
        )
    }

    pub fn apply_enrichment(
        &self,
        id: &str,
        patch: &BookPatch,
        expected_revision: u64,
        status: MetadataStatus,
        confidence: f64,
    ) -> Result<Book> {
        if status != MetadataStatus::Verified
            || !confidence.is_finite()
            || !(0.0..=1.0).contains(&confidence)
        {
            return Err(invalid(
                "Only verified enrichment with real confidence can be applied",
            ));
        }
        if patch.notes.is_some()
            || patch.favorite.is_some()
            || patch.rating.is_some()
            || patch.read_status.is_some()
        {
            return Err(invalid("Enrichment cannot change personal reading fields"));
        }
        self.update_internal(
            id,
            patch,
            expected_revision,
            UpdateContext {
                enrichment: Some((status, confidence)),
                ..UpdateContext::default()
            },
        )
    }

    fn update_internal(
        &self,
        id: &str,
        patch: &BookPatch,
        expected_revision: u64,
        context: UpdateContext<'_>,
    ) -> Result<Book> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| invalid("Library operation is unavailable"))?;
        self.check_cancelled()?;
        let before = self.repository.get(id, &[])?;
        if before.revision != expected_revision {
            return Err(AppError::RevisionConflict);
        }
        if let Some(expected) = context.original_sha256 {
            let original = self
                .repository
                .files(id)?
                .into_iter()
                .find(|file| file.file.variant == FileVariant::Original)
                .ok_or_else(|| AppError::NotFound("Book original".into()))?;
            if !original.file.sha256.eq_ignore_ascii_case(expected) {
                return Err(AppError::Conflict("Inspected original changed".into()));
            }
            // Storage reads through its anchored directory descriptors and enforces the read limit.
            let bytes = self.read_verified_file(&original)?;
            if bytes.len() as u64 != original.file.size_bytes
                || !hex_digest(&bytes).eq_ignore_ascii_case(expected)
            {
                return Err(AppError::Conflict("Inspected original changed".into()));
            }
        }
        if let Some((file_id, expected)) = context.inspected_file {
            let file = self.repository.file_by_id(file_id)?;
            if file.file.book_id != id
                || file.file.format != BookFormat::Epub
                || !file.file.sha256.eq_ignore_ascii_case(expected)
            {
                return Err(AppError::Conflict("Inspected EPUB changed".into()));
            }
            self.read_verified_file(&file)?;
        }
        let before_metadata = book_metadata(&before);
        let mut updated = apply_patch(before.clone(), patch)?;
        let metadata_changed = book_metadata(&updated) != before_metadata;
        if let Some((status, confidence)) = context.enrichment {
            updated.metadata_status = status;
            updated.metadata_confidence = Some(confidence);
        } else if metadata_changed || context.review_job_id.is_some() {
            updated.metadata_status = MetadataStatus::Verified;
            updated.metadata_confidence = None;
        }
        if updated == before && context.review_job_id.is_none() {
            return Ok(self.decorate(before));
        }
        let work = WorkDirectory::new()?;
        let mut catalog_only = false;
        let file = if metadata_changed {
            match self
                .source_epub(id)
                .and_then(|file| self.read_verified_file(&file))
            {
                Ok(bytes) => {
                    match self.normalized_variant(id, &book_metadata(&updated), &bytes, &work) {
                        Ok(file) => file,
                        Err(_) => {
                            catalog_only = true;
                            None
                        }
                    }
                }
                Err(AppError::Unsupported(_)) => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let kind = if catalog_only {
            "metadataUpdateCatalogOnly"
        } else if metadata_changed
            || context.enrichment.is_some()
            || context.review_job_id.is_some()
        {
            "metadataUpdate"
        } else {
            "personalUpdate"
        };
        self.check_cancelled()?;
        Ok(self.decorate(self.repository.update_audited_with_review(
            &updated,
            expected_revision,
            file,
            kind,
            context.review_job_id,
        )?))
    }

    fn read_verified_file(&self, file: &StoredFile) -> Result<Vec<u8>> {
        let bytes = self.storage.read(&file.relative_path)?;
        if bytes.len() as u64 != file.file.size_bytes
            || !hex_digest(&bytes).eq_ignore_ascii_case(&file.file.sha256)
        {
            return Err(AppError::Conflict(
                "The registered book file changed".into(),
            ));
        }
        Ok(bytes)
    }

    pub fn optimize(&self, id: &str, profile_id: &str) -> Result<OptimizationReport> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| invalid("Library operation is unavailable"))?;
        self.check_cancelled()?;
        let profile = optimizer::profile(profile_id)?;
        let book = self.repository.get(id, &[])?;
        let source = self.source_epub(id)?;
        let work = WorkDirectory::new()?;
        let input = work.path.join("input.epub");
        write_stage(&input, &self.storage.read(&source.relative_path)?)?;
        let target = work.path.join("optimized.epub");
        let prospective_id = Uuid::new_v4().to_string();
        self.check_cancelled()?;
        let mut report = optimizer::optimize(&input, &target, &profile, id, &prospective_id)?;
        self.check_cancelled()?;
        let file = self.publish_variant(
            id,
            &book_metadata(&book),
            FileVariant::Optimized,
            BookFormat::Epub,
            Some(profile_id.to_owned()),
            &target,
        )?;
        report.file_id = file.file.id.clone();
        if !self
            .repository
            .files(id)?
            .iter()
            .any(|existing| existing.file.id == file.file.id)
        {
            self.commit_variant(&book, file, "optimize")?;
        }
        Ok(report)
    }

    pub fn convert(&self, id: &str, target: BookFormat) -> Result<ConversionReport> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| invalid("Library operation is unavailable"))?;
        self.check_cancelled()?;
        let book = self.repository.get(id, &[])?;
        let files = self.repository.files(id)?;
        let source = files
            .iter()
            .find(|file| {
                file.file.format == BookFormat::Epub && file.file.variant == FileVariant::Normalized
            })
            .or_else(|| {
                files
                    .iter()
                    .find(|file| file.file.variant == FileVariant::Original)
            })
            .ok_or_else(|| AppError::NotFound("Book source".into()))?;
        let work = WorkDirectory::new()?;
        let input = work
            .path
            .join(format!("input.{}", source.file.format.as_str()));
        write_stage(&input, &self.storage.read(&source.relative_path)?)?;
        let destination = work.path.join(format!("converted.{}", target.as_str()));
        self.check_cancelled()?;
        let report = self
            .converter
            .convert(&input, &destination, target, &book_metadata(&book))?;
        self.check_cancelled()?;
        let file = self.publish_variant(
            id,
            &book_metadata(&book),
            FileVariant::Converted,
            target,
            None,
            &destination,
        )?;
        if !files
            .iter()
            .any(|existing| existing.file.id == file.file.id)
        {
            self.commit_variant(&book, file, "convert")?;
        }
        Ok(report)
    }

    pub fn operations(&self) -> Result<Vec<Operation>> {
        self.repository.operations()
    }

    /// Relocate managed variants to their metadata-derived paths without touching originals.
    pub fn organize(&self, id: &str, expected_revision: u64) -> Result<Book> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| invalid("Library operation is unavailable"))?;
        self.check_cancelled()?;
        let book = self.repository.get(id, &[])?;
        if book.revision != expected_revision {
            return Err(AppError::RevisionConflict);
        }
        let mut files = self.repository.files(id)?;
        if !files
            .iter()
            .any(|file| file.file.variant != FileVariant::Original)
        {
            return Err(AppError::Unsupported(
                "Organization requires a managed variant; original files remain immutable".into(),
            ));
        }
        let metadata = book_metadata(&book);
        let mut changed = false;
        for file in &mut files {
            if file.file.variant == FileVariant::Original {
                continue;
            }
            self.check_cancelled()?;
            let base = self
                .storage
                .managed_book_path(id, &metadata, file.file.format);
            let stem = base
                .strip_suffix(&format!(".{}", file.file.format.as_str()))
                .ok_or_else(|| invalid("Managed variant name is invalid"))?;
            let file_id = Uuid::parse_str(&file.file.id)
                .map_err(|_| invalid("Managed file identifier is invalid"))?;
            let profile = file
                .file
                .profile
                .as_deref()
                .map(|profile| format!("-{}", &hex_digest(profile.as_bytes())[..12]))
                .unwrap_or_default();
            let path = format!(
                "{stem} - {}{profile}-{file_id}.{}",
                file.file.variant.as_str(),
                file.file.format.as_str()
            );
            let source = self.storage.resolve(&file.relative_path)?;
            if Storage::hash_file(&source)? != file.file.sha256 {
                return Err(AppError::Conflict("Managed variant changed".into()));
            }
            if path == file.relative_path {
                continue;
            }
            // Immutable copies retain the old physical asset for undo; only audited paths change.
            let artifact = self.storage.publish_file(&path, &source)?;
            if artifact.sha256 != file.file.sha256 || artifact.size_bytes != file.file.size_bytes {
                return Err(AppError::Conflict("Managed variant changed".into()));
            }
            file.relative_path = artifact.relative_path;
            changed = true;
        }
        self.check_cancelled()?;
        if !changed {
            return Ok(self.decorate(book));
        }
        Ok(self.decorate(self.repository.relocate_files_audited(
            &book,
            expected_revision,
            &files,
        )?))
    }

    /// Catalogue-only removal: immutable bytes remain available for a verified undo.
    pub fn remove_books(
        &self,
        request_id: &str,
        books: &[(String, u64)],
    ) -> Result<Vec<Operation>> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| invalid("Library operation is unavailable"))?;
        self.check_cancelled()?;
        // The repository receipt must handle a retry whose catalogue rows are already gone.
        self.repository.remove_audited(request_id, books)
    }

    pub fn undo(&self, operation_id: &str) -> Result<Operation> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| invalid("Library operation is unavailable"))?;
        self.check_cancelled()?;
        if let Some(files) = self.repository.removal_files_for_undo(operation_id)? {
            for file in files {
                self.check_cancelled()?;
                let path = self.storage.resolve(&file.relative_path)?;
                let before = fs::symlink_metadata(&path)?;
                if !before.is_file() || before.len() != file.file.size_bytes {
                    return Err(AppError::Conflict("A retained book file changed".into()));
                }
                // Source's streaming hash is bounded to 512 MiB and checks path/inode changes.
                // It also supports files above the 64 MiB in-memory reading limit.
                let hash = Storage::hash_file(&path)?;
                let checked = self.storage.resolve(&file.relative_path)?;
                let after = fs::symlink_metadata(&checked)?;
                if !after.is_file()
                    || after.len() != file.file.size_bytes
                    || !hash.eq_ignore_ascii_case(&file.file.sha256)
                {
                    return Err(AppError::Conflict("A retained book file changed".into()));
                }
            }
        }
        self.check_cancelled()?;
        self.repository.undo_audited(operation_id)
    }

    fn source_epub(&self, id: &str) -> Result<StoredFile> {
        let mut files: Vec<_> = self
            .repository
            .files(id)?
            .into_iter()
            .filter(|file| file.file.format == BookFormat::Epub)
            .collect();
        files.sort_by_key(|file| match file.file.variant {
            FileVariant::Normalized => 0,
            FileVariant::Converted => 1,
            FileVariant::Original => 2,
            FileVariant::Optimized => 3,
        });
        files
            .into_iter()
            .next()
            .ok_or_else(|| AppError::Unsupported("This operation requires an EPUB variant".into()))
    }

    fn commit_variant(&self, original: &Book, file: StoredFile, kind: &str) -> Result<()> {
        // Progress can advance independently while a long conversion runs.
        // Rebase only unchanged bibliography, retaining all personal fields.
        for _ in 0..3 {
            self.check_cancelled()?;
            let current = self.repository.get(&original.id, &[])?;
            if book_metadata(&current) != book_metadata(original) {
                return Err(AppError::RevisionConflict);
            }
            self.check_cancelled()?;
            match self.repository.update_audited(
                &current,
                current.revision,
                Some(file.clone()),
                kind,
            ) {
                Ok(_) => return Ok(()),
                Err(AppError::RevisionConflict) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(AppError::RevisionConflict)
    }

    fn normalized_variant(
        &self,
        id: &str,
        metadata: &BookMetadata,
        bytes: &[u8],
        work: &WorkDirectory,
    ) -> Result<Option<StoredFile>> {
        self.check_cancelled()?;
        let mut document = EpubDocument::from_bytes(bytes, &metadata.title)?;
        let before = document.text_fingerprint()?;
        document.update_metadata(metadata)?;
        if before != document.text_fingerprint()? {
            return Err(invalid("Metadata update changed reading text"));
        }
        let output = work
            .path
            .join(format!("normalized-{}.epub", Uuid::new_v4()));
        self.check_cancelled()?;
        document.write(&output, 9)?;
        let file = self.publish_variant(
            id,
            metadata,
            FileVariant::Normalized,
            BookFormat::Epub,
            None,
            &output,
        )?;
        let already_linked = match self.repository.files(id) {
            Ok(files) => files
                .iter()
                .any(|existing| existing.file.id == file.file.id),
            Err(AppError::NotFound(_)) => false,
            Err(error) => return Err(error),
        };
        Ok((!already_linked).then_some(file))
    }

    fn publish_variant(
        &self,
        id: &str,
        metadata: &BookMetadata,
        variant: FileVariant,
        format: BookFormat,
        profile: Option<String>,
        source: &Path,
    ) -> Result<StoredFile> {
        self.check_cancelled()?;
        let hash = Storage::hash_file(source)?;
        if let Some(existing) = self.repository.find_file_by_hash(&hash)? {
            if existing.file.book_id != id {
                return Err(AppError::Conflict(
                    "An identical variant belongs to another book".into(),
                ));
            }
            return Ok(existing);
        }
        let file_id = Uuid::new_v4().to_string();
        let base = self.storage.managed_book_path(id, metadata, format);
        let stem = base
            .strip_suffix(&format!(".{}", format.as_str()))
            .ok_or_else(|| invalid("Managed variant name is invalid"))?;
        let path = format!(
            "{stem} - {}-{}.{}",
            variant.as_str(),
            &file_id[..8],
            format.as_str()
        );
        self.check_cancelled()?;
        let artifact = self.storage.publish_file(&path, source)?;
        let mut file = stored_file(id, variant, format, profile, artifact);
        file.file.id = file_id;
        Ok(file)
    }

    fn thumbnail(&self, archive_bytes: &[u8], path: &str) -> Result<String> {
        self.check_cancelled()?;
        let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes))?;
        let mut entry = archive.by_name(path)?;
        if entry.size() > MAX_COVER_BYTES as u64 {
            return Err(invalid("Cover exceeds the input limit"));
        }
        let mut bytes = Vec::new();
        entry
            .by_ref()
            .take(MAX_COVER_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_COVER_BYTES {
            return Err(invalid("Cover grew beyond its limit"));
        }
        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| invalid("Cover image header is invalid"))?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let image = reader
            .decode()
            .map_err(|_| invalid("Cover cannot be decoded safely"))?
            .thumbnail(256, 384)
            .to_rgb8();
        let mut output = Vec::new();
        JpegEncoder::new_with_quality(&mut output, 80)
            .encode_image(&image)
            .map_err(|_| invalid("Cover thumbnail encoding failed"))?;
        if output.len() > MAX_THUMBNAIL_BYTES {
            return Err(invalid("Cover thumbnail exceeds the output limit"));
        }
        let hash = hex_digest(&output);
        let relative = format!("covers/{hash}.jpg");
        self.check_cancelled()?;
        self.storage.write_new(&relative, &output)?;
        Ok(relative)
    }

    fn decorate(&self, mut book: Book) -> Book {
        book.cover_path = book
            .cover_path
            .as_deref()
            .and_then(|path| self.storage.read(path).ok())
            .filter(|bytes| {
                bytes.len() <= MAX_THUMBNAIL_BYTES && bytes.starts_with(&[0xff, 0xd8, 0xff])
            })
            .map(|bytes| format!("data:image/jpeg;base64,{}", STANDARD.encode(bytes)));
        book
    }
}

fn stored_file(
    book_id: &str,
    variant: FileVariant,
    format: BookFormat,
    profile: Option<String>,
    artifact: StoredArtifact,
) -> StoredFile {
    StoredFile {
        file: BookFile {
            id: Uuid::new_v4().to_string(),
            book_id: book_id.into(),
            variant,
            format,
            profile,
            size_bytes: artifact.size_bytes,
            sha256: artifact.sha256,
            created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true),
        },
        relative_path: artifact.relative_path,
    }
}

fn detect_format(path: &Path) -> Result<BookFormat> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "epub" => Ok(BookFormat::Epub),
        "mobi" | "prc" => Ok(BookFormat::Mobi),
        "azw3" | "azw" | "kf8" => Ok(BookFormat::Azw3),
        "fb2" => Ok(BookFormat::Fb2),
        "txt" => Ok(BookFormat::Txt),
        "html" | "htm" | "xhtml" => Ok(BookFormat::Html),
        "pdf" => Ok(BookFormat::Pdf),
        "cbz" => Ok(BookFormat::Cbz),
        _ => Err(AppError::Unsupported("Unsupported library format".into())),
    }
}

fn book_metadata(book: &Book) -> BookMetadata {
    BookMetadata {
        title: book.title.clone(),
        authors: book.authors.clone(),
        author_sort: book.author_sort.clone(),
        series: book.series.clone(),
        series_index: book.series_index,
        genres: book.genres.clone(),
        tags: book.tags.clone(),
        language: book.language.clone(),
        description: book.description.clone(),
        isbn: book.isbn.clone(),
        publisher: book.publisher.clone(),
        published: book.published.clone(),
    }
}

fn apply_patch(mut book: Book, patch: &BookPatch) -> Result<Book> {
    let mut metadata = book_metadata(&book);
    if let Some(value) = &patch.title {
        metadata.title = value.clone();
    }
    if let Some(value) = &patch.authors {
        metadata.authors = value.clone();
        if patch.author_sort.is_none() {
            metadata.author_sort.clear();
        }
    }
    if let Some(value) = &patch.author_sort {
        metadata.author_sort = value.clone();
    }
    nullable(&mut metadata.series, &patch.series);
    nullable(&mut metadata.series_index, &patch.series_index);
    if let Some(value) = &patch.genres {
        metadata.genres = value.clone();
    }
    if let Some(value) = &patch.tags {
        metadata.tags = value.clone();
    }
    if let Some(value) = &patch.language {
        metadata.language = value.clone();
    }
    if let Some(value) = &patch.description {
        metadata.description = value.clone();
    }
    nullable(&mut metadata.isbn, &patch.isbn);
    nullable(&mut metadata.publisher, &patch.publisher);
    nullable(&mut metadata.published, &patch.published);
    let metadata = normalize_metadata(metadata, true, &mut Vec::new())?;
    book.title = metadata.title;
    book.authors = metadata.authors;
    book.author_sort = metadata.author_sort;
    book.series = metadata.series;
    book.series_index = metadata.series_index;
    book.genres = metadata.genres;
    book.tags = metadata.tags;
    book.language = metadata.language;
    book.description = metadata.description;
    book.isbn = metadata.isbn;
    book.publisher = metadata.publisher;
    book.published = metadata.published;
    if let Some(value) = patch.read_status {
        book.read_status = value;
    }
    if let Some(value) = patch.favorite {
        book.favorite = value;
    }
    nullable(&mut book.rating, &patch.rating);
    if book
        .rating
        .is_some_and(|rating| !rating.is_finite() || !(0.0..=5.0).contains(&rating))
    {
        return Err(invalid("Book rating must be between zero and five"));
    }
    if let Some(value) = &patch.notes {
        book.notes = clean_text(value, 1_048_576)?;
    }
    Ok(book)
}

fn nullable<T: Clone>(target: &mut Option<T>, patch: &Option<Option<T>>) {
    if let Some(value) = patch {
        *target = value.clone();
    }
}
fn invalid(message: &str) -> AppError {
    AppError::InvalidInput(message.into())
}
fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn clean_text(value: &str, maximum: usize) -> Result<String> {
    let result: String = value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect();
    if result.len() > maximum
        || result
            .chars()
            .any(|character| character < ' ' && !matches!(character, '\n' | '\t'))
    {
        return Err(invalid("Book text contains invalid or oversized data"));
    }
    Ok(result.trim().to_owned())
}

fn clean_name(value: &str) -> Result<String> {
    Ok(clean_text(value, 4096)?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" "))
}
fn clean_values(values: Vec<String>, maximum: usize) -> Result<Vec<String>> {
    if values.len() > maximum {
        return Err(invalid("Too many book metadata values"));
    }
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for value in values {
        let value = clean_name(&value)?;
        if !value.is_empty() && seen.insert(value.to_lowercase()) {
            result.push(value);
        }
    }
    Ok(result)
}

fn normalize_metadata(
    mut metadata: BookMetadata,
    strict: bool,
    warnings: &mut Vec<String>,
) -> Result<BookMetadata> {
    metadata.title = clean_name(&metadata.title)?;
    if metadata.title.is_empty() {
        return Err(invalid("Book title is empty"));
    }
    metadata.authors = clean_values(metadata.authors, 64)?;
    metadata.genres = clean_values(metadata.genres, 128)?;
    metadata.tags = clean_values(metadata.tags, 128)?;
    metadata.author_sort = clean_name(&metadata.author_sort)?;
    if metadata.author_sort.is_empty() {
        metadata.author_sort = metadata.authors.first().cloned().unwrap_or_default();
    }
    metadata.series = metadata
        .series
        .as_deref()
        .map(clean_name)
        .transpose()?
        .filter(|value| !value.is_empty());
    if metadata
        .series_index
        .is_some_and(|index| !index.is_finite() || index < 0.0)
    {
        if strict {
            return Err(invalid("Series position must be finite and nonnegative"));
        }
        metadata.series_index = None;
        warnings.push("importInvalidSeriesPosition".into());
    }
    metadata.publisher = metadata
        .publisher
        .as_deref()
        .map(clean_name)
        .transpose()?
        .filter(|value| !value.is_empty());
    metadata.description = clean_text(&metadata.description, 65_536)?;
    metadata.language = clean_text(&metadata.language, 64)?
        .replace('_', "-")
        .to_ascii_lowercase();
    if metadata.language.is_empty() {
        metadata.language = "und".into();
    }
    if !metadata.language.split('-').all(|part| {
        !part.is_empty() && part.len() <= 8 && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }) {
        if strict {
            return Err(invalid("Book language code is invalid"));
        }
        metadata.language = "und".into();
        warnings.push("importInvalidLanguage".into());
    }
    if let Some(value) = metadata
        .isbn
        .take()
        .filter(|value| !value.trim().is_empty())
    {
        match normalized_isbn(&value) {
            Some(value) => metadata.isbn = Some(value),
            None if strict => return Err(invalid("ISBN checksum is invalid")),
            None => warnings.push("importInvalidIsbn".into()),
        }
    }
    if let Some(value) = metadata
        .published
        .take()
        .filter(|value| !value.trim().is_empty())
    {
        match normalized_date(&value) {
            Some(value) => metadata.published = Some(value),
            None if strict => return Err(invalid("Publication date is invalid")),
            None => warnings.push("importInvalidPublicationDate".into()),
        }
    }
    Ok(metadata)
}

fn normalized_isbn(value: &str) -> Option<String> {
    if value.len() > 256 {
        return None;
    }
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    let value = if lower.starts_with("urn:isbn:") {
        &value[9..]
    } else if lower.starts_with("isbn-13:") || lower.starts_with("isbn-10:") {
        &value[8..]
    } else if lower.starts_with("isbn") {
        value[4..].trim_start_matches([' ', ':', '-'])
    } else {
        value
    };
    let digits: String = value
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '-')
        .map(|character| character.to_ascii_uppercase())
        .collect();
    let numbers: Option<Vec<u32>> = digits
        .chars()
        .map(|character| {
            if character == 'X' {
                Some(10)
            } else {
                character.to_digit(10)
            }
        })
        .collect();
    let numbers = numbers?;
    let valid = match numbers.len() {
        10 => {
            numbers[..9].iter().all(|digit| *digit < 10)
                && numbers
                    .iter()
                    .enumerate()
                    .map(|(index, digit)| (10 - index as u32) * digit)
                    .sum::<u32>()
                    .is_multiple_of(11)
        }
        13 => {
            numbers.iter().all(|digit| *digit < 10)
                && numbers
                    .iter()
                    .enumerate()
                    .map(|(index, digit)| {
                        if index.is_multiple_of(2) {
                            *digit
                        } else {
                            3 * digit
                        }
                    })
                    .sum::<u32>()
                    .is_multiple_of(10)
        }
        _ => false,
    };
    valid.then_some(digits)
}

fn normalized_date(value: &str) -> Option<String> {
    if value.len() > 128 {
        return None;
    }
    let value = value.trim();
    if value.len() == 4
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u32>().ok()? > 0
    {
        return Some(value.to_owned());
    }
    if value.len() == 7 && value.as_bytes()[4] == b'-' {
        let year = value[..4].parse().ok()?;
        let month = value[5..].parse().ok()?;
        NaiveDate::from_ymd_opt(year, month, 1)?;
        return Some(value.to_owned());
    }
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Some(date.format("%Y-%m-%d").to_string());
    }
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.format("%Y-%m-%d").to_string())
}

struct WorkDirectory {
    path: PathBuf,
    directory: Option<SecureDir>,
}
impl WorkDirectory {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().canonicalize()?;
        let name = format!("library-manager-library-{}", Uuid::new_v4());
        let parent = SecureDir::open(&root, false, AccessPolicy::Shared)?;
        let directory = parent.child(std::ffi::OsStr::new(&name), true, AccessPolicy::Private)?;
        let path = root.join(name);
        Ok(Self {
            path,
            directory: Some(directory),
        })
    }
}
impl Drop for WorkDirectory {
    fn drop(&mut self) {
        // Release the Windows directory pin before removing the private workspace.
        drop(self.directory.take());
        let _ = fs::remove_dir_all(&self.path);
    }
}
fn write_stage(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::InvalidInput("Missing staging parent".into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| AppError::InvalidInput("Missing staging filename".into()))?;
    let directory = SecureDir::open(parent, false, AccessPolicy::Private)?;
    let mut file = directory.create_new(name, AccessPolicy::Private)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, OperationStatus, ReadStatus};
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use std::{collections::BTreeMap, fs::File};
    use tempfile::TempDir;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    fn replace_fixture_bytes(path: &Path, bytes: &[u8]) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        #[cfg(windows)]
        {
            let retained = path.with_file_name(format!("retained-original-{}", Uuid::new_v4()));
            fs::rename(path, retained).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    fn link_fixture_path(source: &Path, target: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(source, target).unwrap();
        #[cfg(windows)]
        {
            // A junction requires no symlink privilege and must also be refused
            // when it occupies a path where a regular EPUB/TXT file is expected.
            let source = if source.is_dir() {
                source
            } else {
                source.parent().unwrap()
            };
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(target)
                .arg(source)
                .output()
                .unwrap();
            assert!(output.status.success(), "junction fixture creation failed");
        }
    }

    #[test]
    fn working_directory_uses_canonical_system_temp_and_releases_cleanup_handles() {
        let working = WorkDirectory::new().unwrap();
        let path = working.path.clone();
        assert!(path.starts_with(std::env::temp_dir().canonicalize().unwrap()));
        assert!(
            working
                .directory
                .as_ref()
                .unwrap()
                .as_file()
                .metadata()
                .unwrap()
                .is_dir()
        );
        write_stage(&path.join("test.epub"), b"private stage").unwrap();
        assert_eq!(fs::read(path.join("test.epub")).unwrap(), b"private stage");
        assert!(write_stage(&path.join("test.epub"), b"must not replace").is_err());
        drop(working);
        assert!(!path.exists());
    }

    fn fixture() -> (TempDir, LibraryService) {
        let directory = tempfile::Builder::new()
            .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
        let repository =
            BookRepository::new(Database::new(&directory.path().join("database")).unwrap());
        let storage = Storage::new(&directory.path().join("library")).unwrap();
        let service = LibraryService::new(
            storage,
            repository,
            Converter::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
                "../../src-tauri/binaries/library-manager-mobitool-x86_64-unknown-linux-gnu",
            )),
        );
        (directory, service)
    }

    fn seed_pending_review(directory: &TempDir, book: &Book) -> Database {
        let database = Database::new(&directory.path().join("database")).unwrap();
        let proposal = serde_json::json!({"bookId":book.id,"patch":{"title":"Reviewed edition"},"confidence":0.8,"evidence":[{"field":"title","value":"Reviewed edition","confidence":0.8,"sourceUrls":[]}],"warnings":["Manual review"],"providerId":"minimax","modelId":"fixture"});
        let result = serde_json::json!({"proposal":proposal,"review":{"state":"pending","sourceRevision":book.revision,"reviewRevision":book.revision}});
        database.connect().unwrap().execute("INSERT INTO jobs(id,kind,status,progress,payload_json,result_json,created_at,updated_at) VALUES('review-job','enrich','completed',1,?1,?2,'2026-10-10T00:00:00Z','2026-10-10T00:00:00Z')",rusqlite::params![serde_json::json!({"version":1,"payload":{"id":book.id,"bookIds":[book.id]}}).to_string(),result.to_string()]).unwrap();
        database
    }

    fn pending_review_result(database: &Database) -> serde_json::Value {
        let raw: String = database
            .connect()
            .unwrap()
            .query_row(
                "SELECT result_json FROM jobs WHERE id='review-job'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn explicit_review_without_field_changes_is_durable_and_preserves_files() {
        let (directory, service) = fixture();
        let source = write_epub(&directory, false);
        let book = service.import(&source).unwrap().book;
        let database = seed_pending_review(&directory, &book);
        let files = service.repository.files(&book.id).unwrap();
        let operations = service.operations().unwrap().len();
        let updated = service
            .review_book(&book.id, "review-job", &BookPatch::default(), book.revision)
            .unwrap();
        assert_eq!(updated.title, book.title);
        assert_eq!(updated.metadata_status, MetadataStatus::Verified);
        assert_eq!(updated.metadata_confidence, None);
        assert_eq!(updated.revision, book.revision + 1);
        assert_eq!(service.repository.files(&book.id).unwrap(), files);
        assert_eq!(
            pending_review_result(&database)["review"]["state"],
            "applied"
        );
        assert_eq!(service.operations().unwrap().len(), operations + 1);
        let duplicate = service
            .review_book(
                &book.id,
                "review-job",
                &BookPatch::default(),
                updated.revision,
            )
            .unwrap();
        assert_eq!(duplicate.revision, updated.revision);
        assert_eq!(service.operations().unwrap().len(), operations + 1);
        assert_eq!(
            Storage::hash_file(&source).unwrap(),
            files
                .iter()
                .find(|file| file.file.variant == FileVariant::Original)
                .unwrap()
                .file
                .sha256
        );
    }

    #[test]
    fn explicit_review_updates_the_active_epub_and_undo_restores_pending_state() {
        let (directory, service) = fixture();
        let source = write_epub(&directory, false);
        let original_hash = Storage::hash_file(&source).unwrap();
        let book = service.import(&source).unwrap().book;
        let database = seed_pending_review(&directory, &book);
        let original = service
            .repository
            .files(&book.id)
            .unwrap()
            .into_iter()
            .find(|file| file.file.variant == FileVariant::Original)
            .unwrap();
        let updated = service
            .review_book(
                &book.id,
                "review-job",
                &BookPatch {
                    title: Some("Reviewed edition".into()),
                    ..BookPatch::default()
                },
                book.revision,
            )
            .unwrap();
        let active = service.source_epub(&book.id).unwrap();
        let metadata = EpubDocument::from_bytes(
            &service.storage.read(&active.relative_path).unwrap(),
            &updated.title,
        )
        .unwrap()
        .metadata;
        assert_eq!(metadata.title, updated.title);
        assert_eq!(
            pending_review_result(&database)["review"]["state"],
            "applied"
        );
        assert_eq!(Storage::hash_file(&source).unwrap(), original_hash);
        assert_eq!(
            service.storage.read(&original.relative_path).unwrap(),
            fs::read(&source).unwrap()
        );
        let operation = service
            .operations()
            .unwrap()
            .into_iter()
            .find(|operation| operation.kind == "metadataUpdate")
            .unwrap();
        service.undo(&operation.id).unwrap();
        let restored = service.get(&book.id, &[]).unwrap();
        assert_eq!(restored.title, book.title);
        assert_eq!(
            pending_review_result(&database)["review"]["state"],
            "pending"
        );
        assert_eq!(
            pending_review_result(&database)["review"]["reviewRevision"],
            restored.revision
        );
    }

    #[test]
    fn explicit_review_keeps_revision_source_and_cancellation_guards() {
        let (directory, service) = fixture();
        let book = service.import(&write_epub(&directory, false)).unwrap().book;
        let database = seed_pending_review(&directory, &book);
        let original = pending_review_result(&database);
        let count = service.operations().unwrap().len();
        assert!(matches!(
            service.review_book(
                &book.id,
                "review-job",
                &BookPatch::default(),
                book.revision + 1
            ),
            Err(AppError::RevisionConflict)
        ));
        let cancelled = service
            .clone()
            .with_cancellation(Arc::new(AtomicBool::new(true)));
        assert!(matches!(
            cancelled.review_book(&book.id, "review-job", &BookPatch::default(), book.revision),
            Err(AppError::Cancelled)
        ));
        let active = service.source_epub(&book.id).unwrap();
        let path = service.storage.resolve(&active.relative_path).unwrap();
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 1;
        replace_fixture_bytes(&path, &bytes);
        assert!(matches!(
            service.review_book(
                &book.id,
                "review-job",
                &BookPatch {
                    title: Some("Must not apply".into()),
                    ..BookPatch::default()
                },
                book.revision
            ),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(service.get(&book.id, &[]).unwrap().revision, book.revision);
        assert_eq!(pending_review_result(&database), original);
        assert_eq!(service.operations().unwrap().len(), count);
    }

    fn epub_entries(missing_font: bool) -> BTreeMap<String, Vec<u8>> {
        let mut image = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_pixel(5, 3, Rgb([80, 130, 170])))
            .write_to(&mut image, ImageFormat::Png)
            .unwrap();
        BTreeMap::from([
            ("mimetype".into(), b"application/epub+zip".to_vec()),
            ("META-INF/container.xml".into(), br#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_vec()),
            ("OPS/content.opf".into(), format!(r##"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid"><metadata><dc:identifier id="uid">urn:uuid:library-test</dc:identifier><dc:title>Étoiles</dc:title><dc:creator>Auteur</dc:creator><dc:subject>Fantasy</dc:subject><dc:subject>fantasy</dc:subject><dc:language>fr</dc:language><dc:identifier>urn:isbn:9780306406157</dc:identifier><dc:date>2005-05-02</dc:date><meta property="belongs-to-collection" id="series">Saga</meta><meta refines="#series" property="collection-type">series</meta><meta refines="#series" property="group-position">0</meta></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="cover" href="cover.png" media-type="image/png" properties="cover-image"/>{}</manifest><spine><itemref idref="chapter"/></spine></package>"##, if missing_font { r#"<item id="font" href="missing.ttf" media-type="application/x-font-truetype"/>"# } else { "" }).into_bytes()),
            ("OPS/chapter.xhtml".into(), "<html><body><h1>Chapitre</h1><p>Lecture française et 日本語.</p></body></html>".as_bytes().to_vec()),
            ("OPS/cover.png".into(), image.into_inner()),
        ])
    }

    fn write_epub(directory: &TempDir, missing_font: bool) -> PathBuf {
        let path = directory.path().join(if missing_font {
            "warning.epub"
        } else {
            "source.epub"
        });
        let mut entries = epub_entries(missing_font);
        let mut zip = ZipWriter::new(File::create(&path).unwrap());
        zip.start_file(
            "mimetype",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(&entries.remove("mimetype").unwrap()).unwrap();
        for (name, bytes) in entries {
            zip.start_file(
                name,
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn expected_import_hash_rejects_changed_bytes_before_creating_library_records() {
        let (directory, service) = fixture();
        let source = write_epub(&directory, false);
        for invalid_hash in ["", "abc", &"z".repeat(64)] {
            assert!(matches!(
                service.import_expected(&source, invalid_hash),
                Err(AppError::InvalidInput(_))
            ));
        }
        assert!(matches!(
            service.import_expected(&source, &"0".repeat(64)),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(service.list(&BookQuery::default(), &[]).unwrap().total, 0);
        assert!(service.operations().unwrap().is_empty());
        let hash = Storage::hash_file(&source).unwrap();
        assert!(
            service
                .repository
                .find_original_hash(&hash)
                .unwrap()
                .is_none()
        );
        let imported = service.import_expected(&source, &hash).unwrap();
        assert!(!imported.duplicate);
        let duplicate = service
            .import_expected(&source, &hash.to_uppercase())
            .unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.book.id, imported.book.id);
        assert_eq!(service.list(&BookQuery::default(), &[]).unwrap().total, 1);
        assert_eq!(service.operations().unwrap().len(), 1);
    }

    #[test]
    fn import_is_idempotent_classified_and_has_a_private_thumbnail() {
        let (directory, service) = fixture();
        let source = write_epub(&directory, false);
        let original = fs::read(&source).unwrap();
        let outcome = service.import(&source).unwrap();
        assert!(!outcome.duplicate);
        assert!(outcome.warnings.is_empty());
        assert_eq!(outcome.book.title, "Étoiles");
        assert_eq!(outcome.book.genres, vec!["Fantasy"]);
        assert_eq!(outcome.book.series_index, Some(0.0));
        assert!(
            outcome
                .book
                .cover_path
                .as_ref()
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
        let files = service.repository.files(&outcome.book.id).unwrap();
        assert_eq!(files.len(), 2);
        assert!(
            files
                .iter()
                .any(|file| file.file.variant == FileVariant::Normalized
                    && file.relative_path.contains("Auteur/Saga"))
        );
        let duplicated = service.import(&source).unwrap();
        assert!(duplicated.duplicate);
        assert_eq!(duplicated.book.id, outcome.book.id);
        assert_eq!(service.repository.files(&outcome.book.id).unwrap().len(), 2);
        assert_eq!(fs::read(source).unwrap(), original);
        assert_eq!(service.operations().unwrap().len(), 1);
        assert!(!service.operations().unwrap()[0].reversible);
        assert_eq!(
            service
                .repository
                .get(&outcome.book.id, &[])
                .unwrap()
                .cover_path
                .as_ref()
                .unwrap()
                .split('/')
                .next(),
            Some("covers")
        );
    }

    #[test]
    fn structurally_incomplete_and_invalid_epubs_remain_catalogued_with_review_warnings() {
        let (directory, service) = fixture();
        let outcome = service.import(&write_epub(&directory, true)).unwrap();
        assert_eq!(outcome.book.title, "Étoiles");
        assert!(!outcome.warnings.is_empty());
        assert_eq!(outcome.book.metadata_status, MetadataStatus::NeedsReview);
        assert_eq!(service.files(&outcome.book.id).unwrap().len(), 1);
        let broken = directory.path().join("Broken.epub");
        fs::write(&broken, "not a zip").unwrap();
        let outcome = service.import(&broken).unwrap();
        assert_eq!(outcome.book.title, "Broken");
        assert!(
            outcome
                .warnings
                .contains(&"importEpubInspectionFailed".into())
        );
        assert_eq!(fs::read_to_string(broken).unwrap(), "not a zip");
    }

    #[test]
    fn personal_updates_do_not_verify_metadata_and_strict_metadata_keeps_zero_and_fractional_positions()
     {
        let (directory, service) = fixture();
        let source = write_epub(&directory, false);
        let book = service.import(&source).unwrap().book;
        let personal = service
            .update(
                &book.id,
                &BookPatch {
                    notes: Some("Ma note privée".into()),
                    favorite: Some(true),
                    rating: Some(Some(0.0)),
                    ..BookPatch::default()
                },
                book.revision,
            )
            .unwrap();
        assert_eq!(personal.metadata_status, MetadataStatus::Pending);
        assert_eq!(personal.rating, Some(0.0));
        let changed = service
            .update(
                &book.id,
                &BookPatch {
                    series_index: Some(Some(3.5)),
                    tags: Some(vec![" Fantaisie ".into(), "fantaisie".into()]),
                    favorite: Some(false),
                    ..BookPatch::default()
                },
                personal.revision,
            )
            .unwrap();
        assert_eq!(changed.series_index, Some(3.5));
        assert_eq!(changed.tags, vec!["Fantaisie"]);
        assert!(!changed.favorite);
        assert_eq!(changed.notes, "Ma note privée");
        assert_eq!(changed.metadata_status, MetadataStatus::Verified);
        assert_eq!(changed.metadata_confidence, None);
        let zero = service
            .update(
                &book.id,
                &BookPatch {
                    series_index: Some(Some(0.0)),
                    ..BookPatch::default()
                },
                changed.revision,
            )
            .unwrap();
        assert_eq!(zero.series_index, Some(0.0));
        for patch in [
            BookPatch {
                isbn: Some(Some("9780306406158".into())),
                ..BookPatch::default()
            },
            BookPatch {
                published: Some(Some("2025-02-30".into())),
                ..BookPatch::default()
            },
            BookPatch {
                series_index: Some(Some(f64::NAN)),
                ..BookPatch::default()
            },
        ] {
            assert!(service.update(&book.id, &patch, zero.revision).is_err());
        }
        assert_eq!(service.get(&book.id, &[]).unwrap().revision, zero.revision);
        assert_eq!(
            normalized_isbn("ISBN 0-306-40615-2"),
            Some("0306406152".into())
        );
    }

    #[test]
    fn inspected_update_checks_original_hash_and_revision_before_applying_a_patch() {
        let (directory, service) = fixture();
        let source = write_epub(&directory, false);
        let book = service.import(&source).unwrap().book;
        let original = service
            .repository
            .files(&book.id)
            .unwrap()
            .into_iter()
            .find(|file| file.file.variant == FileVariant::Original)
            .unwrap();
        let patch = BookPatch {
            title: Some("Titre relu".into()),
            ..BookPatch::default()
        };
        let operation_count = service.operations().unwrap().len();
        for malformed in ["", "123", &"x".repeat(64)] {
            assert!(matches!(
                service.update_inspected(&book.id, &patch, book.revision, malformed),
                Err(AppError::InvalidInput(_))
            ));
        }
        assert!(matches!(
            service.update_inspected(&book.id, &patch, book.revision, &"0".repeat(64)),
            Err(AppError::Conflict(_))
        ));
        assert!(matches!(
            service.update_inspected(&book.id, &patch, book.revision + 1, &original.file.sha256),
            Err(AppError::RevisionConflict)
        ));
        assert_eq!(service.operations().unwrap().len(), operation_count);
        assert_eq!(service.get(&book.id, &[]).unwrap(), book);
        let updated = service
            .update_inspected(
                &book.id,
                &patch,
                book.revision,
                &original.file.sha256.to_uppercase(),
            )
            .unwrap();
        assert_eq!(updated.title, "Titre relu");
        assert_eq!(updated.revision, book.revision + 1);
        assert_eq!(
            service.storage.read(&original.relative_path).unwrap(),
            fs::read(source).unwrap()
        );
        assert_eq!(
            service
                .repository
                .files(&book.id)
                .unwrap()
                .into_iter()
                .find(|file| file.file.variant == FileVariant::Original)
                .unwrap(),
            original
        );
    }

    #[test]
    fn inspected_update_rejects_corrupted_original_bytes_even_if_catalog_hash_matches() {
        let (directory, service) = fixture();
        let book = service.import(&write_epub(&directory, false)).unwrap().book;
        let original = service
            .repository
            .files(&book.id)
            .unwrap()
            .into_iter()
            .find(|file| file.file.variant == FileVariant::Original)
            .unwrap();
        let path = service.storage.resolve(&original.relative_path).unwrap();
        replace_fixture_bytes(&path, b"Unexpected original bytes");
        let operation_count = service.operations().unwrap().len();
        assert!(matches!(
            service.update_inspected(
                &book.id,
                &BookPatch {
                    notes: Some("Must not apply".into()),
                    ..BookPatch::default()
                },
                book.revision,
                &original.file.sha256
            ),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(service.get(&book.id, &[]).unwrap(), book);
        assert_eq!(service.operations().unwrap().len(), operation_count);
    }

    fn derived_inspection_fixture(
        directory: &TempDir,
        service: &LibraryService,
    ) -> (Book, StoredFile) {
        let txt = directory.path().join("Immutable original.txt");
        fs::write(&txt, "Immutable original edition text").unwrap();
        let book = service.import(&txt).unwrap().book;
        let epub = write_epub(directory, false);
        let artifact = service
            .storage
            .import_original(&epub, BookFormat::Epub)
            .unwrap();
        let converted = stored_file(
            &book.id,
            FileVariant::Converted,
            BookFormat::Epub,
            None,
            artifact,
        );
        service.repository.add_file(converted.clone()).unwrap();
        (service.get(&book.id, &[]).unwrap(), converted)
    }

    #[test]
    fn inspected_derived_epub_update_checks_both_tokens_and_preserves_immutable_txt() {
        let (directory, service) = fixture();
        let (book, converted) = derived_inspection_fixture(&directory, &service);
        let original = service
            .repository
            .files(&book.id)
            .unwrap()
            .into_iter()
            .find(|file| file.file.variant == FileVariant::Original)
            .unwrap();
        let original_bytes = service.storage.read(&original.relative_path).unwrap();
        let converted_bytes = service.storage.read(&converted.relative_path).unwrap();
        let patch = BookPatch {
            title: Some("Reviewed converted edition".into()),
            ..BookPatch::default()
        };
        for (original_hash, inspected_hash) in [
            (&"0".repeat(64), &converted.file.sha256),
            (&original.file.sha256, &"0".repeat(64)),
        ] {
            assert!(matches!(
                service.update_from_inspection(
                    &book.id,
                    &patch,
                    book.revision,
                    original_hash,
                    &converted.file.id,
                    inspected_hash
                ),
                Err(AppError::Conflict(_))
            ));
        }
        let updated = service
            .update_from_inspection(
                &book.id,
                &patch,
                book.revision,
                &original.file.sha256,
                &converted.file.id,
                &converted.file.sha256,
            )
            .unwrap();
        assert_eq!(updated.title, "Reviewed converted edition");
        assert_eq!(updated.revision, book.revision + 1);
        assert_eq!(
            service.storage.read(&original.relative_path).unwrap(),
            original_bytes
        );
        assert_eq!(
            service.storage.read(&converted.relative_path).unwrap(),
            converted_bytes
        );
        assert_eq!(
            service.repository.file_by_id(&original.file.id).unwrap(),
            original
        );
        let normalized = service.source_epub(&book.id).unwrap();
        assert_eq!(normalized.file.variant, FileVariant::Normalized);
        let document = EpubDocument::from_bytes(
            &service.storage.read(&normalized.relative_path).unwrap(),
            "fallback",
        )
        .unwrap();
        assert_eq!(document.metadata.title, updated.title);
    }

    #[test]
    fn corrupt_inspected_derived_epub_is_rejected_before_metadata_or_history_changes() {
        let (directory, service) = fixture();
        let (book, converted) = derived_inspection_fixture(&directory, &service);
        let original = service
            .repository
            .files(&book.id)
            .unwrap()
            .into_iter()
            .find(|file| file.file.variant == FileVariant::Original)
            .unwrap();
        let original_bytes = service.storage.read(&original.relative_path).unwrap();
        let mut corrupt = service.storage.read(&converted.relative_path).unwrap();
        corrupt[0] ^= 1;
        let path = service.storage.resolve(&converted.relative_path).unwrap();
        replace_fixture_bytes(&path, &corrupt);
        let operations = service.operations().unwrap();
        let files = service.repository.files(&book.id).unwrap();
        assert!(matches!(
            service.update_from_inspection(
                &book.id,
                &BookPatch {
                    title: Some("Must not apply".into()),
                    ..BookPatch::default()
                },
                book.revision,
                &original.file.sha256,
                &converted.file.id,
                &converted.file.sha256
            ),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(service.get(&book.id, &[]).unwrap(), book);
        assert_eq!(service.operations().unwrap(), operations);
        assert_eq!(service.repository.files(&book.id).unwrap(), files);
        assert_eq!(
            service.storage.read(&original.relative_path).unwrap(),
            original_bytes
        );
    }

    #[test]
    fn corrupt_active_normalized_epub_cannot_be_used_after_valid_original_inspection() {
        let (directory, service) = fixture();
        let book = service.import(&write_epub(&directory, false)).unwrap().book;
        let files = service.repository.files(&book.id).unwrap();
        let original = files
            .iter()
            .find(|file| file.file.variant == FileVariant::Original)
            .unwrap();
        let normalized = files
            .iter()
            .find(|file| file.file.variant == FileVariant::Normalized)
            .unwrap();
        let original_bytes = service.storage.read(&original.relative_path).unwrap();
        let mut corrupt = service.storage.read(&normalized.relative_path).unwrap();
        corrupt[0] ^= 1;
        let path = service.storage.resolve(&normalized.relative_path).unwrap();
        replace_fixture_bytes(&path, &corrupt);
        let operations = service.operations().unwrap();
        let patch = BookPatch {
            title: Some("Must not publish changed reading text".into()),
            ..BookPatch::default()
        };
        assert!(matches!(
            service.update_from_inspection(
                &book.id,
                &patch,
                book.revision,
                &original.file.sha256,
                &original.file.id,
                &original.file.sha256
            ),
            Err(AppError::Conflict(_))
        ));
        assert!(matches!(
            service.update(&book.id, &patch, book.revision),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(service.get(&book.id, &[]).unwrap(), book);
        assert_eq!(service.operations().unwrap(), operations);
        assert_eq!(service.repository.files(&book.id).unwrap(), files);
        assert_eq!(
            service.storage.read(&original.relative_path).unwrap(),
            original_bytes
        );
    }

    #[test]
    fn organization_preserves_originals_file_identity_and_bytes_and_is_audited_and_idempotent() {
        let (directory, service) = fixture();
        let source = write_epub(&directory, false);
        let source_bytes = fs::read(&source).unwrap();
        let book = service.import(&source).unwrap().book;
        let before = service.repository.files(&book.id).unwrap();
        assert!(
            before
                .iter()
                .any(|file| file.file.variant != FileVariant::Original)
        );
        let operation_count = service.operations().unwrap().len();
        assert!(matches!(
            service.organize(&book.id, book.revision + 1),
            Err(AppError::RevisionConflict)
        ));
        let organized = service.organize(&book.id, book.revision).unwrap();
        let after = service.repository.files(&book.id).unwrap();
        assert_eq!(organized.revision, book.revision + 1);
        assert_eq!(book_metadata(&organized), book_metadata(&book));
        assert_eq!(after.len(), before.len());
        for old in &before {
            let current = after
                .iter()
                .find(|file| file.file.id == old.file.id)
                .unwrap();
            assert_eq!(current.file, old.file);
            assert_eq!(
                service.storage.read(&current.relative_path).unwrap(),
                service.storage.read(&old.relative_path).unwrap()
            );
            if old.file.variant == FileVariant::Original {
                assert_eq!(current.relative_path, old.relative_path);
            } else {
                assert_ne!(current.relative_path, old.relative_path);
                assert!(
                    current
                        .relative_path
                        .starts_with("books/Auteur/Saga/T000 - Étoiles")
                );
            }
        }
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        assert_eq!(service.operations().unwrap().len(), operation_count + 1);
        assert_eq!(
            service.organize(&book.id, organized.revision).unwrap(),
            organized
        );
        assert_eq!(service.operations().unwrap().len(), operation_count + 1);
        let operation = service
            .operations()
            .unwrap()
            .into_iter()
            .find(|operation| operation.kind == "organization")
            .unwrap();
        assert_eq!(
            service.undo(&operation.id).unwrap().status,
            OperationStatus::Reverted
        );
        assert_eq!(service.repository.files(&book.id).unwrap(), before);
        for file in &after {
            assert!(
                service
                    .storage
                    .resolve(&file.relative_path)
                    .unwrap()
                    .is_file()
            );
        }
        let restored = service.get(&book.id, &[]).unwrap();
        service.organize(&book.id, restored.revision).unwrap();
        assert_eq!(service.repository.files(&book.id).unwrap(), after);
    }

    #[test]
    fn organization_requires_a_managed_variant_and_never_relocates_an_original() {
        let (directory, service) = fixture();
        let source = directory.path().join("source.txt");
        fs::write(&source, "Original text").unwrap();
        let book = service.import(&source).unwrap().book;
        let before = service.repository.files(&book.id).unwrap();
        let operation_count = service.operations().unwrap().len();
        assert!(matches!(
            service.organize(&book.id, book.revision),
            Err(AppError::Unsupported(_))
        ));
        assert_eq!(service.repository.files(&book.id).unwrap(), before);
        assert_eq!(service.get(&book.id, &[]).unwrap(), book);
        assert_eq!(service.operations().unwrap().len(), operation_count);
        assert_eq!(fs::read(source).unwrap(), b"Original text");
    }

    #[test]
    fn organization_rejects_corrupted_variant_before_publishing_and_recovers_after_restore() {
        let (directory, service) = fixture();
        let book = service.import(&write_epub(&directory, false)).unwrap().book;
        let before = service.repository.files(&book.id).unwrap();
        let variant = before
            .iter()
            .find(|file| file.file.variant != FileVariant::Original)
            .unwrap();
        let bytes = service.storage.read(&variant.relative_path).unwrap();
        let path = service.storage.resolve(&variant.relative_path).unwrap();
        replace_fixture_bytes(&path, b"Unexpected managed bytes");
        let operation_count = service.operations().unwrap().len();
        assert!(matches!(
            service.organize(&book.id, book.revision),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(service.repository.files(&book.id).unwrap(), before);
        assert_eq!(service.get(&book.id, &[]).unwrap(), book);
        assert_eq!(service.operations().unwrap().len(), operation_count);
        fs::write(path, bytes).unwrap();
        assert_eq!(
            service.organize(&book.id, book.revision).unwrap().revision,
            book.revision + 1
        );
    }

    const REMOVE_REQUEST: &str = "d1879315-08e9-4304-af4a-536c8b3fe8a2";

    #[test]
    fn catalogue_removal_preserves_every_artifact_and_replay_then_undo_restore_associations() {
        let (directory, service) = fixture();
        let source = write_epub(&directory, false);
        let imported = service.import(&source).unwrap().book;
        let book = service
            .update(
                &imported.id,
                &BookPatch {
                    title: Some("Library edition".into()),
                    ..Default::default()
                },
                imported.revision,
            )
            .unwrap();
        let files = service.repository.files(&book.id).unwrap();
        let before: Vec<_> = files
            .iter()
            .map(|file| {
                (
                    file.relative_path.clone(),
                    fs::read(service.storage.resolve(&file.relative_path).unwrap()).unwrap(),
                )
            })
            .collect();
        let request = vec![(book.id.clone(), book.revision)];
        let operations = service.remove_books(REMOVE_REQUEST, &request).unwrap();
        assert_eq!(service.list(&BookQuery::default(), &[]).unwrap().total, 0);
        assert_eq!(
            service.remove_books(REMOVE_REQUEST, &request).unwrap(),
            operations
        );
        for (path, bytes) in &before {
            assert_eq!(
                &fs::read(service.storage.resolve(path).unwrap()).unwrap(),
                bytes
            );
        }
        service.undo(&operations[0].id).unwrap();
        let restored = service.get(&book.id, &[]).unwrap();
        assert!(restored.revision > book.revision);
        assert_eq!(restored.title, book.title);
        assert_eq!(service.repository.files(&book.id).unwrap(), files);
        assert_eq!(
            service.undo(&operations[0].id).unwrap().status,
            OperationStatus::Reverted
        );
        assert_eq!(
            service.get(&book.id, &[]).unwrap().revision,
            restored.revision
        );
        for (path, bytes) in before {
            assert_eq!(
                fs::read(service.storage.resolve(&path).unwrap()).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn removal_undo_rejects_missing_corrupt_or_symlinked_files_without_overwriting_bytes() {
        for damage in [
            "missing",
            "corruptOriginal",
            "corruptActive",
            "symlinkFile",
            "symlinkParent",
        ] {
            let (directory, service) = fixture();
            let source = write_epub(&directory, false);
            let book = service.import(&source).unwrap().book;
            let files = service.repository.files(&book.id).unwrap();
            let chosen = if damage == "corruptOriginal" {
                files
                    .iter()
                    .find(|file| file.file.variant == FileVariant::Original)
                    .unwrap()
            } else {
                files
                    .iter()
                    .find(|file| file.file.variant != FileVariant::Original)
                    .unwrap()
            };
            let path = service.storage.resolve(&chosen.relative_path).unwrap();
            let operation = service
                .remove_books(REMOVE_REQUEST, &[(book.id.clone(), book.revision)])
                .unwrap()
                .remove(0);
            let outside = directory.path().join("untouched-outside.epub");
            let original = fs::read(&path).unwrap();
            fs::write(&outside, &original).unwrap();
            match damage {
                "missing" => fs::remove_file(&path).unwrap(),
                "corruptOriginal" | "corruptActive" => {
                    let mut changed = original.clone();
                    changed[0] ^= 1;
                    replace_fixture_bytes(&path, &changed);
                }
                "symlinkFile" => {
                    fs::remove_file(&path).unwrap();
                    link_fixture_path(&outside, &path);
                }
                "symlinkParent" => {
                    let parent = path.parent().unwrap();
                    let moved = directory.path().join("moved-managed-directory");
                    fs::rename(parent, &moved).unwrap();
                    link_fixture_path(&moved, parent);
                }
                _ => unreachable!(),
            }
            assert!(service.undo(&operation.id).is_err(), "{damage}");
            assert!(matches!(
                service.get(&book.id, &[]),
                Err(AppError::NotFound(_))
            ));
            assert_eq!(
                service
                    .operations()
                    .unwrap()
                    .iter()
                    .find(|candidate| candidate.id == operation.id)
                    .unwrap()
                    .status,
                OperationStatus::Applied
            );
            assert_eq!(fs::read(&outside).unwrap(), original);
        }
    }

    #[test]
    fn catalogue_removal_and_restoration_honor_cancellation() {
        let (directory, service) = fixture();
        let source = write_epub(&directory, false);
        let book = service.import(&source).unwrap().book;
        let cancelled = service
            .clone()
            .with_cancellation(Arc::new(AtomicBool::new(true)));
        let request = vec![(book.id.clone(), book.revision)];
        assert!(matches!(
            cancelled.remove_books(REMOVE_REQUEST, &request),
            Err(AppError::Cancelled)
        ));
        assert_eq!(service.get(&book.id, &[]).unwrap().revision, book.revision);
        let operation = service
            .remove_books(REMOVE_REQUEST, &request)
            .unwrap()
            .remove(0);
        assert!(matches!(
            cancelled.undo(&operation.id),
            Err(AppError::Cancelled)
        ));
        assert_eq!(service.list(&BookQuery::default(), &[]).unwrap().total, 0);
        service.undo(&operation.id).unwrap();
    }

    #[test]
    fn undo_restores_active_file_ids_and_keeps_archived_physical_artifacts() {
        let (directory, service) = fixture();
        let source = write_epub(&directory, false);
        let book = service.import(&source).unwrap().book;
        let before = service.repository.files(&book.id).unwrap();
        service
            .update(
                &book.id,
                &BookPatch {
                    title: Some("Titre récent".into()),
                    ..BookPatch::default()
                },
                book.revision,
            )
            .unwrap();
        let after = service.repository.files(&book.id).unwrap();
        let added = after
            .iter()
            .find(|file| !before.iter().any(|old| old.file.id == file.file.id))
            .unwrap();
        let operation = service
            .operations()
            .unwrap()
            .into_iter()
            .find(|operation| operation.kind == "metadataUpdate")
            .unwrap();
        let reverted = service.undo(&operation.id).unwrap();
        assert_eq!(reverted.status, OperationStatus::Reverted);
        let active = service.repository.files(&book.id).unwrap();
        assert_eq!(active, before);
        assert_eq!(service.get(&book.id, &[]).unwrap().title, "Étoiles");
        assert!(
            service
                .storage
                .resolve(&added.relative_path)
                .unwrap()
                .is_file()
        );
        assert_eq!(
            service.undo(&operation.id).unwrap().status,
            OperationStatus::Reverted
        );
        assert_eq!(
            fs::read(source).unwrap(),
            fs::read(
                service
                    .storage
                    .resolve(
                        &before
                            .iter()
                            .find(|file| file.file.variant == FileVariant::Original)
                            .unwrap()
                            .relative_path
                    )
                    .unwrap()
            )
            .unwrap()
        );
    }

    #[test]
    fn concurrent_progress_invalidates_stale_updates_and_undo_without_losing_personal_data() {
        let (directory, service) = fixture();
        let book = service.import(&write_epub(&directory, false)).unwrap().book;
        let stale = service.repository.get(&book.id, &[]).unwrap();
        service
            .repository
            .save_progress(&book.id, "section:0", 0.4)
            .unwrap();
        let mut changed = stale.clone();
        changed.title = "Must not replace".into();
        assert!(matches!(
            service
                .repository
                .update_audited(&changed, stale.revision, None, "metadataUpdate"),
            Err(AppError::RevisionConflict)
        ));
        let current = service.get(&book.id, &[]).unwrap();
        assert_eq!(current.reading_progress, 0.4);
        assert_eq!(current.read_status, ReadStatus::Reading);
        let changed = service
            .update(
                &book.id,
                &BookPatch {
                    notes: Some("Note".into()),
                    ..BookPatch::default()
                },
                current.revision,
            )
            .unwrap();
        let operation = service
            .operations()
            .unwrap()
            .into_iter()
            .find(|operation| operation.kind == "personalUpdate")
            .unwrap();
        service
            .repository
            .save_progress(&book.id, "section:0", 0.6)
            .unwrap();
        assert!(matches!(
            service.undo(&operation.id),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(service.get(&book.id, &[]).unwrap().notes, changed.notes);
        assert_eq!(service.get(&book.id, &[]).unwrap().reading_progress, 0.6);
    }

    #[test]
    fn optimization_conversion_and_enrichment_are_audited_without_mutating_originals() {
        let (directory, service) = fixture();
        let source = write_epub(&directory, false);
        let original_hash = Storage::hash_file(&source).unwrap();
        let book = service.import(&source).unwrap().book;
        let enriched = service
            .apply_enrichment(
                &book.id,
                &BookPatch {
                    genres: Some(vec!["Science-fiction".into()]),
                    ..BookPatch::default()
                },
                book.revision,
                MetadataStatus::Verified,
                0.96,
            )
            .unwrap();
        assert_eq!(enriched.metadata_confidence, Some(0.96));
        assert!(
            service
                .apply_enrichment(
                    &book.id,
                    &BookPatch {
                        notes: Some("LLM must not write this".into()),
                        ..BookPatch::default()
                    },
                    enriched.revision,
                    MetadataStatus::Verified,
                    0.99
                )
                .is_err()
        );
        assert!(service.optimize(&book.id, "xteink").unwrap().text_preserved);
        assert_eq!(
            service
                .convert(&book.id, BookFormat::Mobi)
                .unwrap()
                .target_format,
            BookFormat::Mobi
        );
        assert!(
            service
                .files(&book.id)
                .unwrap()
                .iter()
                .any(|file| file.format == BookFormat::Mobi)
        );
        assert_eq!(Storage::hash_file(&source).unwrap(), original_hash);
        assert!(
            service
                .operations()
                .unwrap()
                .iter()
                .any(|operation| operation.kind == "convert")
        );
        assert!(
            service
                .operations()
                .unwrap()
                .iter()
                .any(|operation| operation.kind == "optimize")
        );
    }

    #[test]
    fn non_epub_files_are_preserved_and_symlink_import_is_refused() {
        let (directory, service) = fixture();
        let source = directory.path().join("Un texte.txt");
        fs::write(&source, "Café et texte.").unwrap();
        let book = service.import(&source).unwrap().book;
        assert_eq!(book.title, "Un texte");
        assert_eq!(book.language, "und");
        assert!(service.optimize(&book.id, "xteink").is_err());
        service.convert(&book.id, BookFormat::Epub).unwrap();
        assert!(
            service
                .files(&book.id)
                .unwrap()
                .iter()
                .any(|file| file.format == BookFormat::Epub)
        );
        let link = directory.path().join("linked.txt");
        link_fixture_path(&source, &link);
        assert!(service.import(&link).is_err());
        assert_eq!(fs::read_to_string(source).unwrap(), "Café et texte.");
    }

    #[test]
    fn cancellation_refuses_import_publication_and_metadata_without_affecting_other_clones() {
        let (directory, service) = fixture();
        let source = write_epub(&directory, false);
        let signal = Arc::new(AtomicBool::new(true));
        let cancelled = service.clone().with_cancellation(signal.clone());
        assert!(matches!(
            cancelled.import(&source),
            Err(AppError::Cancelled)
        ));
        assert!(service.operations().unwrap().is_empty());
        let book = service.import(&source).unwrap().book;
        let before_files = service.repository.files(&book.id).unwrap();
        let before_operations = service.operations().unwrap();
        assert!(matches!(
            cancelled.update(
                &book.id,
                &BookPatch {
                    title: Some("Cancelled title".into()),
                    ..BookPatch::default()
                },
                book.revision
            ),
            Err(AppError::Cancelled)
        ));
        // A CPU worker may already have produced a private result when cancelled.
        let private_output = directory.path().join("already-computed.txt");
        fs::write(&private_output, "A result that must stay private").unwrap();
        assert!(matches!(
            cancelled.publish_variant(
                &book.id,
                &book_metadata(&book),
                FileVariant::Converted,
                BookFormat::Txt,
                None,
                &private_output
            ),
            Err(AppError::Cancelled)
        ));
        assert!(matches!(
            cancelled.optimize(&book.id, "xteink"),
            Err(AppError::Cancelled)
        ));
        assert!(matches!(
            cancelled.convert(&book.id, BookFormat::Mobi),
            Err(AppError::Cancelled)
        ));
        assert_eq!(service.repository.files(&book.id).unwrap(), before_files);
        assert_eq!(service.operations().unwrap(), before_operations);
        assert_eq!(
            service.repository.get(&book.id, &[]).unwrap().title,
            "Étoiles"
        );
        signal.store(false, Ordering::Release);
        let applied = cancelled
            .update(
                &book.id,
                &BookPatch {
                    favorite: Some(true),
                    ..BookPatch::default()
                },
                book.revision,
            )
            .unwrap();
        assert!(applied.favorite);
    }
}
