use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path};
use std::str::FromStr;

use chrono::{SecondsFormat, Utc};
use rusqlite::types::{Type, Value};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
use uuid::Uuid;

use crate::database::Database;
use crate::error::{AppError, Result};
use crate::models::{
    Book, BookFile, BookMetadata, BookPage, BookQuery, BookSort, Facet, FileVariant, LibraryFacets,
    MetadataProposal, MetadataStatus, Operation, OperationStatus, ReadStatus,
};

const MAX_PAGE_SIZE: u64 = 200;
const MAX_FILTER_VALUES: usize = 200;
const MAX_CONNECTED_DEVICES: usize = 256;
const MAX_REVIEW_RESULT_BYTES: usize = 1024 * 1024;
const MAX_REMOVAL_BOOKS: usize = 200;
const MAX_OPERATION_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const MAX_REMOVAL_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
const BOOK_FROM: &str = "books b LEFT JOIN book_files selected_file ON selected_file.id = (
    SELECT preferred.id FROM book_files preferred WHERE preferred.book_id = b.id
    ORDER BY CASE WHEN preferred.variant = 'original' THEN 0 ELSE 1 END,
    preferred.created_at DESC, preferred.id DESC LIMIT 1)";
const BOOK_SELECT: &str = "b.*, selected_file.format AS selected_format,
    selected_file.size_bytes AS selected_size_bytes";

/// Internal storage record. The relative path must not be returned as an IPC file capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredFile {
    #[serde(flatten)]
    pub file: BookFile,
    pub relative_path: String,
}

#[derive(Debug, Clone)]
pub struct BookRepository {
    database: Database,
}

impl BookRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn list(&self, query: &BookQuery, connected_device_ids: &[String]) -> Result<BookPage> {
        validate_query(query, connected_device_ids)?;
        let limit = query.limit.clamp(1, MAX_PAGE_SIZE);
        let (filter, values) = query_filter(query, connected_device_ids);
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction()?;
        let total: u64 = transaction.query_row(
            &format!("SELECT count(*) FROM {BOOK_FROM} WHERE {filter}"),
            params_from_iter(values.iter()),
            |row| checked_unsigned(row.get(0)?, 0),
        )?;
        let mut page_values = values;
        page_values.push(Value::Integer(to_sql_integer(limit, "page size")?));
        page_values.push(Value::Integer(to_sql_integer(query.offset, "page offset")?));
        let sql = format!(
            "SELECT {BOOK_SELECT} FROM {BOOK_FROM} WHERE {filter} ORDER BY {} LIMIT ? OFFSET ?",
            sort_expression(query.sort, query.descending)
        );
        let mut items = transaction
            .prepare(&sql)?
            .query_map(params_from_iter(page_values.iter()), map_book)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        populate_presence(&transaction, &mut items, connected_device_ids)?;
        transaction.commit()?;
        Ok(BookPage {
            items,
            total,
            offset: query.offset,
            limit,
        })
    }

    pub fn get(&self, id: &str, connected_device_ids: &[String]) -> Result<Book> {
        validate_id(id)?;
        validate_devices(connected_device_ids)?;
        let connection = self.database.connect()?;
        let mut book = get_book(&connection, id)?;
        populate_presence(
            &connection,
            std::slice::from_mut(&mut book),
            connected_device_ids,
        )?;
        Ok(book)
    }

    pub fn facets(&self, connected_device_ids: &[String]) -> Result<LibraryFacets> {
        validate_devices(connected_device_ids)?;
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction()?;
        let mut facets = LibraryFacets {
            authors: json_facets(&transaction, "authors_json")?,
            series: scalar_facets(&transaction, "series")?,
            genres: json_facets(&transaction, "genres_json")?,
            tags: json_facets(&transaction, "tags_json")?,
            languages: scalar_facets(&transaction, "language")?,
            formats: query_facets(
                &transaction,
                "SELECT format, count(DISTINCT book_id) FROM book_files GROUP BY format",
                &[],
            )?,
            devices: Vec::new(),
        };
        if !connected_device_ids.is_empty() {
            let values: Vec<Value> = connected_device_ids
                .iter()
                .cloned()
                .map(Value::Text)
                .collect();
            facets.devices = query_facets(
                &transaction,
                &format!(
                    "SELECT device_id, count(DISTINCT book_id) FROM device_books
                    WHERE book_id IS NOT NULL AND device_id IN ({}) GROUP BY device_id",
                    placeholders(values.len())
                ),
                &values,
            )?;
        }
        transaction.commit()?;
        Ok(facets)
    }

    pub fn insert(
        &self,
        id: &str,
        metadata: BookMetadata,
        files: &[StoredFile],
        cover_relative: Option<&str>,
    ) -> Result<Book> {
        validate_id(id)?;
        validate_metadata(&metadata)?;
        if let Some(path) = cover_relative {
            validate_relative_path(path)?;
        }
        for file in files {
            validate_file(file)?;
            if file.file.book_id != id {
                return Err(AppError::InvalidInput(
                    "A file belongs to a different book".into(),
                ));
            }
        }
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists = book_exists(&transaction, id)?;
        if exists {
            for file in files {
                let owner = hash_owner(&transaction, &file.file.sha256)?;
                if owner.as_deref() != Some(id) {
                    return Err(AppError::Conflict(
                        "The book id already belongs to another import".into(),
                    ));
                }
            }
        } else {
            insert_metadata(&transaction, id, &metadata, cover_relative)?;
            for file in files {
                insert_file(&transaction, file)?;
            }
            sync_search(&transaction, id)?;
        }
        let book = get_book(&transaction, id)?;
        transaction.commit()?;
        Ok(book)
    }

    pub fn update_book(&self, book: &Book, expected_revision: u64) -> Result<Book> {
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut updated = persist_book(&transaction, book, expected_revision)?;
        populate_presence(
            &transaction,
            std::slice::from_mut(&mut updated),
            &book.on_device_ids,
        )?;
        transaction.commit()?;
        Ok(updated)
    }

    /// Metadata, active variants, search index and history commit together.
    pub fn update_audited(
        &self,
        book: &Book,
        expected_revision: u64,
        file: Option<StoredFile>,
        kind: &str,
    ) -> Result<Book> {
        self.update_audited_with_review(book, expected_revision, file, kind, None)
    }

    /// Human review resolution, metadata, variants and history share the same CAS transaction.
    pub fn update_audited_with_review(
        &self,
        book: &Book,
        expected_revision: u64,
        file: Option<StoredFile>,
        kind: &str,
        review_job_id: Option<&str>,
    ) -> Result<Book> {
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = self.update_audited_in_transaction(
            &transaction,
            book,
            expected_revision,
            file,
            kind,
            review_job_id,
        )?;
        transaction.commit()?;
        Ok(updated)
    }

    /// Join the caller's transaction so metadata, review and job completion share one commit.
    #[allow(clippy::too_many_arguments)] // Explicit audited inputs plus the caller-owned transaction.
    pub(crate) fn update_audited_in_transaction(
        &self,
        connection: &Connection,
        book: &Book,
        expected_revision: u64,
        file: Option<StoredFile>,
        kind: &str,
        review_job_id: Option<&str>,
    ) -> Result<Book> {
        if connection.is_autocommit() {
            return Err(AppError::Conflict(
                "Audited update requires a transaction".into(),
            ));
        }
        validate_id(kind)?;
        if let Some(id) = review_job_id {
            validate_id(id)?;
        }
        let mut before = audit_snapshot(connection, &book.id)?;
        if before.book.revision != expected_revision || book.revision != expected_revision {
            return Err(AppError::RevisionConflict);
        }
        let reviews = proposal_records(connection, &book.id)?;
        if let Some(id) = review_job_id {
            let target = reviews
                .iter()
                .find(|record| record.id == id)
                .ok_or_else(|| AppError::NotFound("Metadata proposal".into()))?;
            if review_state(&target.result) == Some("applied")
                && target.result["review"]["resolvedRevision"].as_u64()
                    == Some(before.book.revision)
                && book == &before.book
                && file.is_none()
            {
                return Ok(before.book);
            }
            require_pending_review(&target.result, expected_revision)?;
        }
        let updated = persist_book(connection, book, expected_revision)?;
        if let Some(file) = file {
            validate_file(&file)?;
            if file.file.book_id != book.id {
                return Err(AppError::InvalidInput(
                    "A variant belongs to a different book".into(),
                ));
            }
            insert_file(connection, &file)?;
        }
        let mut after = audit_snapshot(connection, &book.id)?;
        let changed = !same_bibliographic_metadata(&before.book, &updated);
        for record in reviews {
            let explicit = review_job_id == Some(record.id.as_str());
            let pending = review_state(&record.result).is_none()
                || review_state(&record.result) == Some("pending");
            if !pending {
                continue;
            }
            let mut result = review_envelope(&record.result);
            let mut review = result
                .get("review")
                .and_then(JsonValue::as_object)
                .cloned()
                .unwrap_or_default();
            if explicit || changed {
                let stale = review
                    .get("reviewRevision")
                    .and_then(JsonValue::as_u64)
                    .is_some_and(|revision| revision != expected_revision);
                review.insert(
                    "state".into(),
                    JsonValue::from(if explicit {
                        "applied"
                    } else if stale {
                        "obsolete"
                    } else {
                        "dismissed"
                    }),
                );
                review.insert("resolvedRevision".into(), JsonValue::from(updated.revision));
            } else if review.get("reviewRevision").and_then(JsonValue::as_u64)
                == Some(expected_revision)
            {
                review.insert("reviewRevision".into(), JsonValue::from(updated.revision));
            } else {
                continue;
            }
            result["review"] = JsonValue::Object(review);
            before
                .reviews
                .push(review_snapshot(&record.id, &record.result)?);
            after.reviews.push(review_snapshot(&record.id, &result)?);
            write_review_result(connection, &record.id, &result)?;
        }
        insert_operation(connection, kind, Some(&before), &after)?;
        Ok(updated)
    }

    /// Audit path changes while preserving every stored file identity and the original assets.
    pub fn relocate_files_audited(
        &self,
        book: &Book,
        expected_revision: u64,
        files: &[StoredFile],
    ) -> Result<Book> {
        validate_book(book)?;
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut before = audit_snapshot(&transaction, &book.id)?;
        if before.book.revision != expected_revision || book.revision != expected_revision {
            return Err(AppError::RevisionConflict);
        }
        if book != &before.book || files.len() != before.files.len() {
            return Err(AppError::InvalidInput(
                "Organization can only relocate the existing book files".into(),
            ));
        }
        let existing: BTreeMap<_, _> = before
            .files
            .iter()
            .map(|file| (file.file.id.as_str(), file))
            .collect();
        let mut seen_ids = HashSet::new();
        let mut seen_paths = HashSet::new();
        let mut changed = false;
        for file in files {
            validate_file(file)?;
            if !seen_ids.insert(file.file.id.as_str())
                || !seen_paths.insert(file.relative_path.as_str())
            {
                return Err(AppError::InvalidInput(
                    "Organization contains duplicate files or paths".into(),
                ));
            }
            let current = existing.get(file.file.id.as_str()).ok_or_else(|| {
                AppError::InvalidInput("Organization contains an unknown file".into())
            })?;
            if current.file != file.file {
                return Err(AppError::InvalidInput(
                    "Organization cannot change file identity".into(),
                ));
            }
            if current.relative_path == file.relative_path {
                continue;
            }
            if file.file.variant == FileVariant::Original {
                return Err(AppError::InvalidInput(
                    "Original files cannot be relocated".into(),
                ));
            }
            if !file.relative_path.starts_with("books/") {
                return Err(AppError::InvalidInput(
                    "Organized variants must remain in managed book storage".into(),
                ));
            }
            let occupied: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM book_files WHERE relative_path=? AND id<>?)",
                params![file.relative_path, file.file.id],
                |row| row.get(0),
            )?;
            if occupied {
                return Err(AppError::Conflict(
                    "The organized path belongs to another file".into(),
                ));
            }
            changed = true;
        }
        if !changed {
            transaction.commit()?;
            return Ok(before.book);
        }
        let updated = persist_book(&transaction, book, expected_revision)?;
        for file in files {
            let current = existing[file.file.id.as_str()];
            if current.relative_path == file.relative_path {
                continue;
            }
            if transaction.execute(
                "UPDATE book_files SET relative_path=? WHERE id=? AND book_id=? AND relative_path=?",
                params![file.relative_path, file.file.id, book.id, current.relative_path],
            )? != 1 {
                return Err(AppError::Conflict("The managed file changed during organization".into()));
            }
        }
        let mut after = audit_snapshot(&transaction, &book.id)?;
        for record in proposal_records(&transaction, &book.id)? {
            if review_state(&record.result) != Some("pending")
                || record.result["review"]["reviewRevision"].as_u64() != Some(expected_revision)
            {
                continue;
            }
            let mut result = record.result.clone();
            result["review"]["reviewRevision"] = JsonValue::from(updated.revision);
            before
                .reviews
                .push(review_snapshot(&record.id, &record.result)?);
            after.reviews.push(review_snapshot(&record.id, &result)?);
            write_review_result(&transaction, &record.id, &result)?;
        }
        insert_operation(&transaction, "organization", Some(&before), &after)?;
        transaction.commit()?;
        Ok(updated)
    }

    pub fn insert_audited(
        &self,
        id: &str,
        metadata: BookMetadata,
        files: &[StoredFile],
        cover_relative: Option<&str>,
    ) -> Result<Book> {
        validate_id(id)?;
        validate_metadata(&metadata)?;
        if let Some(path) = cover_relative {
            validate_relative_path(path)?;
        }
        for file in files {
            validate_file(file)?;
            if file.file.book_id != id {
                return Err(AppError::InvalidInput(
                    "A file belongs to a different book".into(),
                ));
            }
        }
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if book_exists(&transaction, id)? {
            return Err(AppError::Conflict("Book import already exists".into()));
        }
        insert_metadata(&transaction, id, &metadata, cover_relative)?;
        for file in files {
            insert_file(&transaction, file)?;
        }
        sync_search(&transaction, id)?;
        let after = audit_snapshot(&transaction, id)?;
        insert_operation(&transaction, "import", None, &after)?;
        transaction.commit()?;
        Ok(after.book)
    }

    pub fn operations(&self) -> Result<Vec<Operation>> {
        let connection = self.database.connect()?;
        let records = connection.prepare("SELECT id,kind,status,CASE WHEN before_json IS NULL THEN NULL ELSE '' END AS before_json,NULL AS after_json,COALESCE(json_extract(after_json,'$.book.title'),'') AS book_title,created_at FROM operations ORDER BY created_at DESC,id DESC LIMIT 500")?
            .query_map([], operation_record)?.collect::<rusqlite::Result<Vec<_>>>()?;
        records.into_iter().map(public_operation).collect()
    }

    /// Remove catalogue rows as one reversible, revision-bound request; never remove bytes.
    pub fn remove_audited(
        &self,
        request_id: &str,
        books: &[(String, u64)],
    ) -> Result<Vec<Operation>> {
        let request_books = validate_removal_request(request_id, books)?;
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = removal_receipts(&transaction, request_id)?;
        if !previous.is_empty() {
            let mut operations = Vec::with_capacity(previous.len());
            if previous.len() != request_books.len() {
                return Err(AppError::Conflict(
                    "Removal request receipt is incomplete".into(),
                ));
            }
            for record in previous {
                let receipt: RemovalReceipt = serde_json::from_str(
                    record
                        .after
                        .as_deref()
                        .ok_or_else(|| AppError::Conflict("Removal receipt is missing".into()))?,
                )?;
                if receipt.request_id != request_id
                    || receipt.books != request_books
                    || record.status != "applied"
                {
                    return Err(AppError::Conflict(
                        "Removal request was changed or already undone".into(),
                    ));
                }
                operations.push(public_operation(record)?);
            }
            transaction.commit()?;
            return Ok(operations);
        }
        let mut snapshots = Vec::with_capacity(request_books.len());
        let mut total_bytes = 0usize;
        // Check the whole batch before deleting even the first book.
        for (id, revision) in &request_books {
            let snapshot = removal_snapshot(&transaction, id)?;
            if snapshot.audit.book.revision != *revision {
                return Err(AppError::RevisionConflict);
            }
            require_no_active_book_jobs(&transaction, id)?;
            let receipt = RemovalReceipt {
                book: snapshot.audit.book.clone(),
                request_id: request_id.into(),
                books: request_books.clone(),
            };
            let before = bounded_snapshot(&snapshot)?;
            let after = bounded_snapshot(&receipt)?;
            total_bytes = total_bytes
                .saturating_add(before.len())
                .saturating_add(after.len());
            if total_bytes > MAX_REMOVAL_SNAPSHOT_BYTES {
                return Err(AppError::InvalidInput(
                    "Removal snapshots exceed the batch history limit".into(),
                ));
            }
            snapshots.push((id.clone(), *revision, before, after));
        }
        let mut operations = Vec::with_capacity(snapshots.len());
        for (book_id, revision, before, after) in snapshots {
            let operation_id = removal_operation_id(request_id, &book_id)?;
            let timestamp = now();
            transaction.execute("INSERT INTO operations(id,kind,status,before_json,after_json,created_at) VALUES(?1,'catalogueRemove','applied',?2,?3,?4)", params![operation_id,before,after,timestamp])?;
            transaction.execute("DELETE FROM books_search WHERE book_id=?", [&book_id])?;
            let changed = transaction.execute(
                "DELETE FROM books WHERE id=?1 AND revision=?2",
                params![book_id, revision as i64],
            )?;
            require_changed(&transaction, &book_id, changed)?;
            let receipt: RemovalReceipt = serde_json::from_str(&after)?;
            operations.push(public_operation(OperationRecord {
                id: operation_id,
                kind: "catalogueRemove".into(),
                status: "applied".into(),
                before: Some(before),
                after: Some(after),
                title: receipt.book.title,
                created_at: timestamp,
            })?);
        }
        transaction.commit()?;
        Ok(operations)
    }

    /// Paths stay private; LibraryService verifies each retained file before undoing a removal.
    pub fn removal_files_for_undo(&self, operation_id: &str) -> Result<Option<Vec<StoredFile>>> {
        validate_id(operation_id)?;
        let connection = self.database.connect()?;
        let record = load_operation(&connection, operation_id)?;
        if record.kind != "catalogueRemove" || record.status == "reverted" {
            return Ok(None);
        }
        let (snapshot, _) = decode_removal(&record)?;
        Ok(Some(snapshot.audit.files))
    }

    pub fn undo_audited(&self, operation_id: &str) -> Result<Operation> {
        validate_id(operation_id)?;
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = transaction.query_row("SELECT id,kind,status,before_json,after_json,COALESCE(json_extract(after_json,'$.book.title'),'') AS book_title,created_at FROM operations WHERE id=?", [operation_id], operation_record)
            .optional()?.ok_or_else(|| AppError::NotFound("Operation".into()))?;
        if record.status == "reverted" {
            return public_operation(record);
        }
        if record.status != "applied" {
            return Err(AppError::Conflict("Operation cannot be undone".into()));
        }
        if record.kind == "catalogueRemove" {
            restore_catalogue_removal(&transaction, &record)?;
            transaction.execute(
                "UPDATE operations SET status='reverted' WHERE id=? AND status='applied'",
                [operation_id],
            )?;
            let mut reverted = record;
            reverted.status = "reverted".into();
            transaction.commit()?;
            return public_operation(reverted);
        }
        let before: AuditSnapshot =
            serde_json::from_str(record.before.as_deref().ok_or_else(|| {
                AppError::Unsupported("Import history is not reversible".into())
            })?)?;
        let after: AuditSnapshot = serde_json::from_str(
            record
                .after
                .as_deref()
                .ok_or_else(|| AppError::Conflict("Operation snapshot is missing".into()))?,
        )?;
        let current = audit_snapshot(&transaction, &after.book.id)?;
        if current.book.revision != after.book.revision
            || current.files != after.files
            || before.book.id != after.book.id
        {
            return Err(AppError::Conflict(
                "The book or its variants changed after this operation".into(),
            ));
        }
        for file in &before.files {
            validate_file(file)?;
            let after_file = after
                .files
                .iter()
                .find(|after| after.file.id == file.file.id)
                .ok_or_else(|| AppError::Conflict("An original file snapshot is missing".into()))?;
            if file.file != after_file.file
                || (file.file.variant == FileVariant::Original
                    && file.relative_path != after_file.relative_path)
            {
                return Err(AppError::Conflict(
                    "Operation file identity is inconsistent".into(),
                ));
            }
        }
        validate_review_undo(&transaction, &before, &after)?;
        let restored = persist_book(&transaction, &before.book, current.book.revision)?;
        let before_ids: std::collections::HashSet<_> = before
            .files
            .iter()
            .map(|file| file.file.id.as_str())
            .collect();
        for file in &after.files {
            if !before_ids.contains(file.file.id.as_str()) {
                transaction.execute(
                    "DELETE FROM book_files WHERE id=? AND book_id=?",
                    params![file.file.id, after.book.id],
                )?;
            }
        }
        for file in &before.files {
            let after_file = after
                .files
                .iter()
                .find(|after| after.file.id == file.file.id)
                .ok_or_else(|| AppError::Conflict("Operation file snapshot is missing".into()))?;
            if file.relative_path == after_file.relative_path {
                continue;
            }
            let occupied: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM book_files WHERE relative_path=? AND id<>?)",
                params![file.relative_path, file.file.id],
                |row| row.get(0),
            )?;
            if occupied {
                return Err(AppError::Conflict(
                    "The previous path belongs to another file".into(),
                ));
            }
            if transaction.execute(
                "UPDATE book_files SET relative_path=? WHERE id=? AND book_id=? AND relative_path=?",
                params![file.relative_path, file.file.id, before.book.id, after_file.relative_path],
            )? != 1 {
                return Err(AppError::Conflict("The managed file changed during undo".into()));
            }
        }
        restore_review_undo(&transaction, &before, restored.revision)?;
        let mut reverted = record;
        transaction.execute(
            "UPDATE operations SET status='reverted' WHERE id=? AND status='applied'",
            [operation_id],
        )?;
        reverted.status = "reverted".into();
        transaction.commit()?;
        public_operation(reverted)
    }

    pub fn files(&self, id: &str) -> Result<Vec<StoredFile>> {
        validate_id(id)?;
        let connection = self.database.connect()?;
        if !book_exists(&connection, id)? {
            return Err(AppError::NotFound("Book".into()));
        }
        Ok(connection
            .prepare(
                "SELECT * FROM book_files WHERE book_id=? ORDER BY
            CASE WHEN variant='original' THEN 0 ELSE 1 END, created_at DESC, id DESC",
            )?
            .query_map([id], map_file)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn file_by_id(&self, id: &str) -> Result<StoredFile> {
        validate_id(id)?;
        self.database
            .connect()?
            .query_row("SELECT * FROM book_files WHERE id=?", [id], map_file)
            .optional()?
            .ok_or_else(|| AppError::NotFound("Book file".into()))
    }

    pub fn add_file(&self, file: StoredFile) -> Result<()> {
        validate_file(&file)?;
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !book_exists(&transaction, &file.file.book_id)? {
            return Err(AppError::NotFound("Book".into()));
        }
        if insert_file(&transaction, &file)? {
            transaction.execute(
                "UPDATE books SET revision=revision+1, updated_at=? WHERE id=?",
                params![now(), file.file.book_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn find_original_by_hash(&self, hash: &str) -> Result<Option<Book>> {
        validate_hash(hash)?;
        let connection = self.database.connect()?;
        let id: Option<String> = connection
            .query_row(
                "SELECT book_id FROM book_files WHERE sha256=? AND variant='original'",
                [hash.to_ascii_lowercase()],
                |row| row.get(0),
            )
            .optional()?;
        id.map(|id| get_book(&connection, &id)).transpose()
    }

    pub fn find_original_hash(&self, hash: &str) -> Result<Option<StoredFile>> {
        validate_hash(hash)?;
        Ok(self
            .database
            .connect()?
            .query_row(
                "SELECT * FROM book_files WHERE sha256=? AND variant='original'",
                [hash.to_ascii_lowercase()],
                map_file,
            )
            .optional()?)
    }

    pub fn find_file_by_hash(&self, hash: &str) -> Result<Option<StoredFile>> {
        validate_hash(hash)?;
        Ok(self
            .database
            .connect()?
            .query_row(
                "SELECT * FROM book_files WHERE sha256=?",
                [hash.to_ascii_lowercase()],
                map_file,
            )
            .optional()?)
    }

    pub fn set_metadata_status(
        &self,
        id: &str,
        status: MetadataStatus,
        confidence: Option<f64>,
    ) -> Result<()> {
        validate_id(id)?;
        validate_range(confidence, 0.0, 1.0, "metadata confidence")?;
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE books SET metadata_status=?1, metadata_confidence=?2, revision=revision+1, updated_at=?3
            WHERE id=?4 AND (metadata_status<>?1 OR metadata_confidence IS NOT ?2)",
            params![status.as_str(), confidence, now(), id],
        )?;
        require_existing(&transaction, id, changed)?;
        transaction.commit()?;
        Ok(())
    }

    /// Revision-bound status changes cannot overwrite human edits or reading progress.
    pub fn set_metadata_status_if_revision(
        &self,
        id: &str,
        status: MetadataStatus,
        confidence: Option<f64>,
        expected_revision: u64,
    ) -> Result<()> {
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.set_metadata_status_in_transaction(
            &transaction,
            id,
            status,
            confidence,
            expected_revision,
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Return the revision actually committed by the caller, never an inferred later revision.
    pub(crate) fn set_metadata_status_in_transaction(
        &self,
        connection: &Connection,
        id: &str,
        status: MetadataStatus,
        confidence: Option<f64>,
        expected_revision: u64,
    ) -> Result<Book> {
        if connection.is_autocommit() {
            return Err(AppError::Conflict(
                "Metadata publication requires a transaction".into(),
            ));
        }
        validate_id(id)?;
        validate_range(confidence, 0.0, 1.0, "metadata confidence")?;
        if expected_revision == 0 || expected_revision >= i64::MAX as u64 {
            return Err(AppError::InvalidInput("Invalid book revision".into()));
        }
        let current = get_book(connection, id)?;
        if current.revision != expected_revision {
            return Err(AppError::RevisionConflict);
        }
        if current.metadata_status != status || current.metadata_confidence != confidence {
            let changed = connection.execute(
                "UPDATE books SET metadata_status=?1, metadata_confidence=?2, revision=revision+1, updated_at=?3 WHERE id=?4 AND revision=?5",
                params![status.as_str(), confidence, now(), id, expected_revision as i64],
            )?;
            require_changed(connection, id, changed)?;
            rebase_pending_reviews(connection, id, current.revision, current.revision + 1)?;
        }
        get_book(connection, id)
    }

    pub fn save_progress(&self, id: &str, location: &str, progress: f64) -> Result<()> {
        validate_id(id)?;
        validate_range(Some(progress), 0.0, 1.0, "reading progress")?;
        if location.len() > 8192 || location.contains('\0') {
            return Err(AppError::InvalidInput(
                "Reader location is too large or invalid".into(),
            ));
        }
        let status = if progress >= 1.0 {
            ReadStatus::Finished
        } else if progress > 0.0 {
            ReadStatus::Reading
        } else {
            ReadStatus::Unread
        };
        let mut connection = self.database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous_revision = get_book(&transaction, id)?.revision;
        let changed = transaction.execute(
            "UPDATE books SET reader_location=?1, reading_progress=?2, read_status=?3, revision=revision+1, updated_at=?4
            WHERE id=?5 AND (reader_location IS NOT ?1 OR reading_progress<>?2 OR read_status<>?3)",
            params![location, progress, status.as_str(), now(), id],
        )?;
        require_existing(&transaction, id, changed)?;
        if changed > 0 {
            rebase_pending_reviews(&transaction, id, previous_revision, previous_revision + 1)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn reader_location(&self, id: &str) -> Result<Option<String>> {
        validate_id(id)?;
        self.database
            .connect()?
            .query_row(
                "SELECT reader_location FROM books WHERE id=?",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| AppError::NotFound("Book".into()))
    }
}

fn get_book(connection: &Connection, id: &str) -> Result<Book> {
    connection
        .query_row(
            &format!("SELECT {BOOK_SELECT} FROM {BOOK_FROM} WHERE b.id=?"),
            [id],
            map_book,
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound("Book".into()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuditSnapshot {
    book: Book,
    files: Vec<StoredFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    reviews: Vec<ReviewSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewSnapshot {
    job_id: String,
    review: Option<JsonValue>,
    legacy: bool,
    fingerprint: String,
}

struct ProposalRecord {
    id: String,
    result: JsonValue,
}

fn audit_snapshot(connection: &Connection, id: &str) -> Result<AuditSnapshot> {
    Ok(AuditSnapshot {
        book: get_book(connection, id)?,
        files: connection
            .prepare("SELECT * FROM book_files WHERE book_id=? ORDER BY id")?
            .query_map([id], map_file)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        reviews: Vec::new(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemovalSnapshot {
    #[serde(flatten)]
    audit: AuditSnapshot,
    reader_location: Option<String>,
    device_links: Vec<DeviceLinkSnapshot>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemovalReceipt {
    book: Book,
    request_id: String,
    books: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeviceLinkSnapshot {
    device_id: String,
    relative_path: String,
    book_id: Option<String>,
    sha256: Option<String>,
    title: String,
    authors_json: String,
    format: String,
    size_bytes: u64,
    last_seen_at: String,
}

fn validate_removal_request(
    request_id: &str,
    books: &[(String, u64)],
) -> Result<Vec<(String, u64)>> {
    let valid_request = Uuid::parse_str(request_id)
        .ok()
        .is_some_and(|id| id.to_string() == request_id);
    if !valid_request || books.is_empty() || books.len() > MAX_REMOVAL_BOOKS {
        return Err(AppError::InvalidInput(
            "Removal needs a canonical request UUID and one to 200 books".into(),
        ));
    }
    let mut unique = HashSet::new();
    for (id, revision) in books {
        validate_id(id)?;
        if *revision == 0 || *revision >= i64::MAX as u64 || !unique.insert(id) {
            return Err(AppError::InvalidInput(
                "Removal needs distinct books and valid revisions".into(),
            ));
        }
    }
    let mut ordered = books.to_vec();
    ordered.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(ordered)
}

fn bounded_snapshot(value: &impl Serialize) -> Result<String> {
    let encoded = serde_json::to_string(value)?;
    if encoded.len() > MAX_OPERATION_SNAPSHOT_BYTES {
        return Err(AppError::InvalidInput(
            "Operation snapshot exceeds the history limit".into(),
        ));
    }
    Ok(encoded)
}

fn removal_operation_id(request_id: &str, book_id: &str) -> Result<String> {
    let bytes = serde_json::to_vec(&(request_id, book_id))?;
    let hash = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("catalogue-remove-{hash}"))
}

fn load_operation(connection: &Connection, id: &str) -> Result<OperationRecord> {
    connection.query_row("SELECT id,kind,status,before_json,after_json,COALESCE(json_extract(after_json,'$.book.title'),'') AS book_title,created_at FROM operations WHERE id=?", [id], operation_record)
        .optional()?.ok_or_else(|| AppError::NotFound("Operation".into()))
}

fn removal_receipts(connection: &Connection, request_id: &str) -> Result<Vec<OperationRecord>> {
    Ok(connection.prepare("SELECT id,kind,status,before_json,after_json,COALESCE(json_extract(after_json,'$.book.title'),'') AS book_title,created_at FROM operations WHERE kind='catalogueRemove' AND json_extract(after_json,'$.requestId')=? ORDER BY json_extract(after_json,'$.book.id')")?
        .query_map([request_id], operation_record)?.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn require_no_active_book_jobs(connection: &Connection, book_id: &str) -> Result<()> {
    let active: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE status IN ('queued','running','waitingForConfiguration','waitingForNetwork') AND (json_extract(payload_json,'$.payload.id')=?1 OR json_extract(payload_json,'$.payload.bookId')=?1 OR EXISTS(SELECT 1 FROM json_each(payload_json,'$.bookIds') WHERE type='text' AND value=?1) OR EXISTS(SELECT 1 FROM json_each(payload_json,'$.payload.bookIds') WHERE type='text' AND value=?1) OR EXISTS(SELECT 1 FROM json_each(payload_json,'$.payload.selectedBookIds') WHERE type='text' AND value=?1)))", [book_id], |row| row.get(0))?;
    if active {
        return Err(AppError::Conflict(
            "A selected book has active background work".into(),
        ));
    }
    Ok(())
}

fn map_device_link(row: &Row<'_>) -> rusqlite::Result<DeviceLinkSnapshot> {
    Ok(DeviceLinkSnapshot {
        device_id: row.get("device_id")?,
        relative_path: row.get("relative_path")?,
        book_id: row.get("book_id")?,
        sha256: row.get("sha256")?,
        title: row.get("title")?,
        authors_json: row.get("authors_json")?,
        format: row.get("format")?,
        size_bytes: checked_unsigned(row.get("size_bytes")?, 0)?,
        last_seen_at: row.get("last_seen_at")?,
    })
}

fn removal_snapshot(connection: &Connection, id: &str) -> Result<RemovalSnapshot> {
    Ok(RemovalSnapshot {
        audit: audit_snapshot(connection, id)?,
        reader_location: connection.query_row(
            "SELECT reader_location FROM books WHERE id=?",
            [id],
            |row| row.get(0),
        )?,
        device_links: connection
            .prepare("SELECT * FROM device_books WHERE book_id=? ORDER BY device_id,relative_path")?
            .query_map([id], map_device_link)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    })
}

fn decode_removal(record: &OperationRecord) -> Result<(RemovalSnapshot, RemovalReceipt)> {
    if record.status != "applied" {
        return Err(AppError::Conflict("Removal cannot be undone".into()));
    }
    let snapshot: RemovalSnapshot = serde_json::from_str(
        record
            .before
            .as_deref()
            .ok_or_else(|| AppError::Conflict("Removal snapshot is missing".into()))?,
    )?;
    let receipt: RemovalReceipt = serde_json::from_str(
        record
            .after
            .as_deref()
            .ok_or_else(|| AppError::Conflict("Removal receipt is missing".into()))?,
    )?;
    let request = validate_removal_request(&receipt.request_id, &receipt.books)?;
    if request != receipt.books
        || snapshot.audit.book != receipt.book
        || record.id != removal_operation_id(&receipt.request_id, &receipt.book.id)?
        || !request.contains(&(receipt.book.id.clone(), receipt.book.revision))
    {
        return Err(AppError::Conflict(
            "Removal snapshots are inconsistent".into(),
        ));
    }
    validate_book(&snapshot.audit.book)?;
    if snapshot
        .reader_location
        .as_ref()
        .is_some_and(|value| value.len() > 8192 || value.contains('\0'))
    {
        return Err(AppError::Conflict("Reader snapshot is invalid".into()));
    }
    for file in &snapshot.audit.files {
        validate_file(file)?;
        if file.file.book_id != snapshot.audit.book.id {
            return Err(AppError::Conflict(
                "Removal file belongs to another book".into(),
            ));
        }
    }
    for link in &snapshot.device_links {
        validate_id(&link.device_id)?;
        validate_relative_path(&link.relative_path)?;
        if link.book_id.as_deref() != Some(snapshot.audit.book.id.as_str()) {
            return Err(AppError::Conflict(
                "Removal device link belongs to another book".into(),
            ));
        }
    }
    Ok((snapshot, receipt))
}

fn restore_catalogue_removal(connection: &Connection, record: &OperationRecord) -> Result<()> {
    let (snapshot, _) = decode_removal(record)?;
    let book = &snapshot.audit.book;
    if book_exists(connection, &book.id)? {
        return Err(AppError::Conflict(
            "The removed book identity has been reused".into(),
        ));
    }
    require_no_active_book_jobs(connection, &book.id)?;
    for file in &snapshot.audit.files {
        let occupied: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM book_files WHERE id=?1 OR relative_path=?2 OR sha256=?3)",
            params![
                file.file.id,
                file.relative_path,
                file.file.sha256.to_ascii_lowercase()
            ],
            |row| row.get(0),
        )?;
        if occupied {
            return Err(AppError::Conflict(
                "A removed file identity belongs to another book".into(),
            ));
        }
    }
    for link in &snapshot.device_links {
        let current = connection
            .query_row(
                "SELECT * FROM device_books WHERE device_id=?1 AND relative_path=?2",
                params![link.device_id, link.relative_path],
                map_device_link,
            )
            .optional()?;
        let mut detached = link.clone();
        detached.book_id = None;
        if current.as_ref() != Some(&detached) {
            return Err(AppError::Conflict(
                "The device inventory changed since catalogue removal".into(),
            ));
        }
    }
    let metadata = BookMetadata {
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
    };
    insert_metadata(connection, &book.id, &metadata, book.cover_path.as_deref())?;
    for file in &snapshot.audit.files {
        insert_file(connection, file)?;
    }
    persist_book(connection, book, 1)?;
    let revision = book.revision + 1;
    connection.execute(
        "UPDATE books SET revision=?1,added_at=?2,reader_location=?3 WHERE id=?4",
        params![
            revision as i64,
            book.added_at,
            snapshot.reader_location,
            book.id
        ],
    )?;
    for link in &snapshot.device_links {
        let changed = connection.execute("UPDATE device_books SET book_id=?1 WHERE device_id=?2 AND relative_path=?3 AND book_id IS NULL",params![book.id,link.device_id,link.relative_path])?;
        if changed != 1 {
            return Err(AppError::Conflict(
                "The device link changed during undo".into(),
            ));
        }
    }
    rebase_pending_reviews(connection, &book.id, book.revision, revision)?;
    Ok(())
}

fn same_bibliographic_metadata(left: &Book, right: &Book) -> bool {
    left.title == right.title
        && left.authors == right.authors
        && left.author_sort == right.author_sort
        && left.series == right.series
        && left.series_index == right.series_index
        && left.genres == right.genres
        && left.tags == right.tags
        && left.language == right.language
        && left.description == right.description
        && left.isbn == right.isbn
        && left.publisher == right.publisher
        && left.published == right.published
}

fn proposal_records(connection: &Connection, book_id: &str) -> Result<Vec<ProposalRecord>> {
    let mut statement = connection.prepare("SELECT id,result_json FROM jobs WHERE kind='enrich' AND status='completed' AND length(result_json)<=?2 AND CASE WHEN json_valid(result_json) THEN COALESCE(json_extract(result_json,'$.proposal.bookId'),json_extract(result_json,'$.bookId')) END=?1 ORDER BY created_at,id")?;
    let rows = statement.query_map(params![book_id, MAX_REVIEW_RESULT_BYTES as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut records = Vec::new();
    for row in rows {
        let (id, raw) = row?;
        let Ok(result) = serde_json::from_str::<JsonValue>(&raw) else {
            continue;
        };
        let proposal = result.get("proposal").unwrap_or(&result);
        if serde_json::from_value::<MetadataProposal>(proposal.clone())
            .is_ok_and(|proposal| proposal.book_id == book_id)
        {
            records.push(ProposalRecord { id, result });
        }
    }
    Ok(records)
}

fn review_state(result: &JsonValue) -> Option<&str> {
    result.get("review")?;
    Some(result["review"]["state"].as_str().unwrap_or("invalid"))
}

fn require_pending_review(result: &JsonValue, revision: u64) -> Result<()> {
    if !matches!(review_state(result), None | Some("pending")) {
        return Err(AppError::Conflict(
            "Metadata proposal is already resolved or invalid".into(),
        ));
    }
    if let Some(review) = result.get("review") {
        let pending_revision = review
            .get("reviewRevision")
            .and_then(JsonValue::as_u64)
            .filter(|value| *value > 0)
            .ok_or_else(|| AppError::Conflict("Metadata proposal revision is invalid".into()))?;
        if pending_revision != revision {
            return Err(AppError::RevisionConflict);
        }
        if review.get("sourceRevision").is_some_and(|value| {
            value
                .as_u64()
                .is_none_or(|source| source == 0 || source > pending_revision)
        }) {
            return Err(AppError::Conflict(
                "Metadata proposal source revision is invalid".into(),
            ));
        }
    }
    Ok(())
}

fn review_envelope(result: &JsonValue) -> JsonValue {
    if result.get("proposal").is_some() {
        result.clone()
    } else {
        serde_json::json!({"proposal":result})
    }
}

fn review_snapshot(id: &str, result: &JsonValue) -> Result<ReviewSnapshot> {
    let mut body = review_envelope(result);
    body.as_object_mut()
        .ok_or_else(|| AppError::Conflict("Metadata proposal is invalid".into()))?
        .remove("review");
    let fingerprint = Sha256::digest(serde_json::to_vec(&body)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(ReviewSnapshot {
        job_id: id.into(),
        review: result.get("review").cloned(),
        legacy: result.get("proposal").is_none(),
        fingerprint,
    })
}

fn write_review_result(connection: &Connection, id: &str, result: &JsonValue) -> Result<()> {
    let raw = serde_json::to_string(result)?;
    if raw.len() > MAX_REVIEW_RESULT_BYTES {
        return Err(AppError::InvalidInput(
            "Metadata review result exceeds its storage limit".into(),
        ));
    }
    if connection.execute(
        "UPDATE jobs SET result_json=?2 WHERE id=?1 AND kind='enrich' AND status='completed'",
        params![id, raw],
    )? != 1
    {
        return Err(AppError::Conflict("Metadata proposal changed".into()));
    }
    Ok(())
}

fn rebase_pending_reviews(
    connection: &Connection,
    id: &str,
    previous: u64,
    updated: u64,
) -> Result<()> {
    for record in proposal_records(connection, id)? {
        if review_state(&record.result) != Some("pending")
            || record.result["review"]["reviewRevision"].as_u64() != Some(previous)
        {
            continue;
        }
        let mut result = record.result;
        result["review"]["reviewRevision"] = JsonValue::from(updated);
        write_review_result(connection, &record.id, &result)?;
    }
    Ok(())
}

fn validate_review_undo(
    connection: &Connection,
    before: &AuditSnapshot,
    after: &AuditSnapshot,
) -> Result<()> {
    if before.reviews.len() != after.reviews.len() {
        return Err(AppError::Conflict(
            "Metadata review history is inconsistent".into(),
        ));
    }
    if before.reviews.is_empty() {
        return Ok(());
    }
    let records = proposal_records(connection, &after.book.id)?;
    for (previous, expected) in before.reviews.iter().zip(&after.reviews) {
        if previous.job_id != expected.job_id || previous.fingerprint != expected.fingerprint {
            return Err(AppError::Conflict(
                "Metadata review history is inconsistent".into(),
            ));
        }
        let record = records
            .iter()
            .find(|record| record.id == expected.job_id)
            .ok_or_else(|| AppError::Conflict("Metadata proposal history is missing".into()))?;
        let current = review_snapshot(&record.id, &record.result)?;
        if current.fingerprint != expected.fingerprint
            || current.review != expected.review
            || current.legacy != expected.legacy
        {
            return Err(AppError::Conflict(
                "Metadata proposal changed after this operation".into(),
            ));
        }
    }
    Ok(())
}

fn restore_review_undo(
    connection: &Connection,
    before: &AuditSnapshot,
    revision: u64,
) -> Result<()> {
    if before.reviews.is_empty() {
        return Ok(());
    }
    let records = proposal_records(connection, &before.book.id)?;
    for snapshot in &before.reviews {
        let record = records
            .iter()
            .find(|record| record.id == snapshot.job_id)
            .ok_or_else(|| AppError::Conflict("Metadata proposal history is missing".into()))?;
        let mut result = review_envelope(&record.result);
        if let Some(mut review) = snapshot.review.clone() {
            if review["state"] == "pending"
                && review["reviewRevision"].as_u64() == Some(before.book.revision)
            {
                review["reviewRevision"] = JsonValue::from(revision);
            }
            result["review"] = review;
        } else {
            result.as_object_mut().unwrap().remove("review");
        }
        if snapshot.legacy {
            result = result["proposal"].clone();
        }
        write_review_result(connection, &snapshot.job_id, &result)?;
    }
    Ok(())
}

fn persist_book(connection: &Connection, book: &Book, expected_revision: u64) -> Result<Book> {
    validate_book(book)?;
    let expected_revision = to_sql_integer(expected_revision, "revision")?;
    if expected_revision < 1 || expected_revision == i64::MAX {
        return Err(AppError::InvalidInput("Invalid expected revision".into()));
    }
    let changed = connection.execute(
        "UPDATE books SET title=?1, authors_json=?2, author_sort=?3, series=?4,
        series_index=?5, genres_json=?6, tags_json=?7, language=?8, description=?9,
        isbn=?10, publisher=?11, published=?12, cover_relative_path=?13, read_status=?14,
        reading_progress=?15, favorite=?16, rating=?17, notes=?18, metadata_status=?19,
        metadata_confidence=?20, revision=revision+1, updated_at=?21 WHERE id=?22 AND revision=?23",
        params![
            book.title,
            serde_json::to_string(&book.authors)?,
            effective_author_sort(&book.author_sort, &book.authors),
            book.series,
            book.series_index,
            serde_json::to_string(&book.genres)?,
            serde_json::to_string(&book.tags)?,
            book.language,
            book.description,
            book.isbn,
            book.publisher,
            book.published,
            book.cover_path,
            book.read_status.as_str(),
            book.reading_progress,
            book.favorite,
            book.rating,
            book.notes,
            book.metadata_status.as_str(),
            book.metadata_confidence,
            now(),
            book.id,
            expected_revision
        ],
    )?;
    require_changed(connection, &book.id, changed)?;
    sync_search(connection, &book.id)?;
    get_book(connection, &book.id)
}

fn insert_operation(
    connection: &Connection,
    kind: &str,
    before: Option<&AuditSnapshot>,
    after: &AuditSnapshot,
) -> Result<()> {
    let before_json = before.map(serde_json::to_string).transpose()?;
    let after_json = serde_json::to_string(after)?;
    if before_json
        .as_ref()
        .is_some_and(|value| value.len() > 8 * 1024 * 1024)
        || after_json.len() > 8 * 1024 * 1024
    {
        return Err(AppError::InvalidInput(
            "Operation snapshot exceeds the history limit".into(),
        ));
    }
    connection.execute("INSERT INTO operations(id,kind,status,before_json,after_json,created_at) VALUES(?,?,'applied',?,?,?)",
        params![Uuid::new_v4().to_string(), kind, before_json, after_json, now()])?;
    Ok(())
}

struct OperationRecord {
    id: String,
    kind: String,
    status: String,
    before: Option<String>,
    after: Option<String>,
    title: String,
    created_at: String,
}

fn operation_record(row: &Row<'_>) -> rusqlite::Result<OperationRecord> {
    Ok(OperationRecord {
        id: row.get("id")?,
        kind: row.get("kind")?,
        status: row.get("status")?,
        before: row.get("before_json")?,
        after: row.get("after_json")?,
        title: row.get("book_title")?,
        created_at: row.get("created_at")?,
    })
}

fn public_operation(record: OperationRecord) -> Result<Operation> {
    let status = OperationStatus::from_str(&record.status)
        .map_err(|_| AppError::Conflict("Operation status is invalid".into()))?;
    Ok(Operation {
        id: record.id,
        description: record.title,
        kind: record.kind,
        reversible: record.before.is_some() && status == OperationStatus::Applied,
        status,
        created_at: record.created_at,
    })
}

fn book_exists(connection: &Connection, id: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM books WHERE id=?)",
        [id],
        |row| row.get(0),
    )?)
}

fn require_changed(connection: &Connection, id: &str, changed: usize) -> Result<()> {
    if changed > 0 {
        return Ok(());
    }
    if book_exists(connection, id)? {
        Err(AppError::RevisionConflict)
    } else {
        Err(AppError::NotFound("Book".into()))
    }
}

fn require_existing(connection: &Connection, id: &str, changed: usize) -> Result<()> {
    if changed > 0 || book_exists(connection, id)? {
        Ok(())
    } else {
        Err(AppError::NotFound("Book".into()))
    }
}

fn insert_metadata(
    connection: &Connection,
    id: &str,
    metadata: &BookMetadata,
    cover: Option<&str>,
) -> Result<()> {
    let timestamp = now();
    connection.execute(
        "INSERT INTO books(id,title,authors_json,author_sort,series,series_index,genres_json,tags_json,
        language,description,isbn,publisher,published,cover_relative_path,added_at,updated_at)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        params![id, metadata.title, serde_json::to_string(&metadata.authors)?,
            effective_author_sort(&metadata.author_sort, &metadata.authors), metadata.series,
            metadata.series_index, serde_json::to_string(&metadata.genres)?, serde_json::to_string(&metadata.tags)?,
            metadata.language, metadata.description, metadata.isbn, metadata.publisher, metadata.published,
            cover, timestamp, timestamp],
    )?;
    Ok(())
}

fn insert_file(connection: &Connection, stored: &StoredFile) -> Result<bool> {
    let file = &stored.file;
    if let Some(owner) = hash_owner(connection, &file.sha256)? {
        if owner == file.book_id {
            return Ok(false);
        }
        return Err(AppError::Conflict(
            "Identical file content already belongs to another book".into(),
        ));
    }
    let collision: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM book_files WHERE id=? OR relative_path=?)",
        params![file.id, stored.relative_path],
        |row| row.get(0),
    )?;
    if collision {
        return Err(AppError::Conflict(
            "The file id or library path is already in use".into(),
        ));
    }
    let timestamp = if file.created_at.is_empty() {
        now()
    } else {
        file.created_at.clone()
    };
    connection.execute(
        "INSERT INTO book_files(id,book_id,format,variant,profile,relative_path,sha256,size_bytes,created_at)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![file.id, file.book_id, file.format.as_str(), file.variant.as_str(), file.profile,
            stored.relative_path, file.sha256.to_ascii_lowercase(), to_sql_integer(file.size_bytes, "file size")?, timestamp],
    )?;
    Ok(true)
}

fn hash_owner(connection: &Connection, hash: &str) -> Result<Option<String>> {
    Ok(connection
        .query_row(
            "SELECT book_id FROM book_files WHERE sha256=?",
            [hash.to_ascii_lowercase()],
            |row| row.get(0),
        )
        .optional()?)
}

fn sync_search(connection: &Connection, id: &str) -> Result<()> {
    let book = get_book(connection, id)?;
    let genres = book
        .genres
        .iter()
        .chain(&book.tags)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    connection.execute("DELETE FROM books_search WHERE book_id=?", [id])?;
    connection.execute(
        "INSERT INTO books_search(book_id,title,authors,series,genres,description) VALUES(?,?,?,?,?,?)",
        params![id, book.title, book.authors.join(" "), book.series.unwrap_or_default(), genres, book.description],
    )?;
    Ok(())
}

fn map_book(row: &Row<'_>) -> rusqlite::Result<Book> {
    let selected_format: Option<String> = row.get("selected_format")?;
    Ok(Book {
        id: row.get("id")?,
        title: row.get("title")?,
        authors: json_array(row, "authors_json")?,
        author_sort: row.get("author_sort")?,
        series: row.get("series")?,
        series_index: row.get("series_index")?,
        genres: json_array(row, "genres_json")?,
        tags: json_array(row, "tags_json")?,
        language: row.get("language")?,
        description: row.get("description")?,
        isbn: row.get("isbn")?,
        publisher: row.get("publisher")?,
        published: row.get("published")?,
        cover_path: row.get("cover_relative_path")?,
        format: parse_enum(
            row,
            "selected_format",
            selected_format.as_deref().unwrap_or("epub"),
        )?,
        size_bytes: checked_unsigned(
            row.get::<_, Option<i64>>("selected_size_bytes")?
                .unwrap_or_default(),
            row.as_ref().column_index("selected_size_bytes")?,
        )?,
        added_at: row.get("added_at")?,
        updated_at: row.get("updated_at")?,
        read_status: parse_enum(row, "read_status", &row.get::<_, String>("read_status")?)?,
        reading_progress: row.get("reading_progress")?,
        favorite: row.get("favorite")?,
        rating: row.get("rating")?,
        notes: row.get("notes")?,
        metadata_status: parse_enum(
            row,
            "metadata_status",
            &row.get::<_, String>("metadata_status")?,
        )?,
        metadata_confidence: row.get("metadata_confidence")?,
        revision: unsigned_column(row, "revision")?,
        on_device_ids: Vec::new(),
    })
}

fn map_file(row: &Row<'_>) -> rusqlite::Result<StoredFile> {
    Ok(StoredFile {
        file: BookFile {
            id: row.get("id")?,
            book_id: row.get("book_id")?,
            format: parse_enum(row, "format", &row.get::<_, String>("format")?)?,
            variant: parse_enum(row, "variant", &row.get::<_, String>("variant")?)?,
            profile: row.get("profile")?,
            size_bytes: unsigned_column(row, "size_bytes")?,
            sha256: row.get("sha256")?,
            created_at: row.get("created_at")?,
        },
        relative_path: row.get("relative_path")?,
    })
}

fn parse_enum<T: FromStr<Err = String>>(
    row: &Row<'_>,
    column: &str,
    value: &str,
) -> rusqlite::Result<T> {
    value.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            row.as_ref().column_index(column).unwrap_or_default(),
            Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        )
    })
}

fn json_array(row: &Row<'_>, column: &str) -> rusqlite::Result<Vec<String>> {
    let value: String = row.get(column)?;
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            row.as_ref().column_index(column).unwrap_or_default(),
            Type::Text,
            Box::new(error),
        )
    })
}

fn unsigned_column(row: &Row<'_>, column: &str) -> rusqlite::Result<u64> {
    let index = row.as_ref().column_index(column)?;
    checked_unsigned(row.get(index)?, index)
}

fn checked_unsigned(value: i64, index: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn populate_presence(
    connection: &Connection,
    books: &mut [Book],
    devices: &[String],
) -> Result<()> {
    if books.is_empty() || devices.is_empty() {
        return Ok(());
    }
    let mut values: Vec<Value> = books
        .iter()
        .map(|book| Value::Text(book.id.clone()))
        .collect();
    values.extend(devices.iter().cloned().map(Value::Text));
    let sql = format!(
        "SELECT DISTINCT book_id, device_id FROM device_books WHERE book_id IN ({})
        AND device_id IN ({}) ORDER BY device_id",
        placeholders(books.len()),
        placeholders(devices.len())
    );
    let pairs = connection
        .prepare(&sql)?
        .query_map(params_from_iter(values.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut presence: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (book, device) in pairs {
        presence.entry(book).or_default().push(device);
    }
    for book in books {
        book.on_device_ids = presence.remove(&book.id).unwrap_or_default();
    }
    Ok(())
}

fn query_filter(query: &BookQuery, connected_devices: &[String]) -> (String, Vec<Value>) {
    let mut conditions = Vec::new();
    let mut values = Vec::new();
    if !query.search.trim().is_empty() {
        // These predicates read authoritative columns so existing FTS rows need no rebuild.
        let mut alternatives: Vec<String> = Vec::new();
        if let Some(search) = literal_search(&query.search) {
            alternatives.push(
                "b.id IN (SELECT book_id FROM books_search WHERE books_search MATCH ?)".into(),
            );
            values.push(Value::Text(search));
        }
        alternatives.push("b.publisher LIKE ? ESCAPE '\\'".into());
        alternatives.push("b.notes LIKE ? ESCAPE '\\'".into());
        let pattern = literal_like(&query.search);
        values.push(Value::Text(pattern.clone()));
        values.push(Value::Text(pattern));
        let compact: String = query
            .search
            .nfkc()
            .filter(|character| {
                !character.is_whitespace() && !matches!(character, '-' | '‐' | '‑' | '–' | '−')
            })
            .collect();
        if (3..=13).contains(&compact.len())
            && compact
                .chars()
                .all(|character| character.is_ascii_digit() || matches!(character, 'x' | 'X'))
        {
            alternatives.push("REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(COALESCE(b.isbn,''),'-',''),' ',''),char(9),''),char(10),''),char(13),''),'‐',''),'‑',''),'–',''),'−','') LIKE ? ESCAPE '\\'".into());
            values.push(Value::Text(format!("%{compact}%")));
        }
        conditions.push(format!("({})", alternatives.join(" OR ")));
    }
    for (column, selected) in [
        ("authors_json", &query.authors),
        ("genres_json", &query.genres),
        ("tags_json", &query.tags),
    ] {
        if !selected.is_empty() {
            conditions.push(format!(
                "EXISTS(SELECT 1 FROM json_each(b.{column}) item WHERE item.value IN ({}))",
                placeholders(selected.len())
            ));
            values.extend(selected.iter().cloned().map(Value::Text));
        }
    }
    for (column, selected) in [("series", &query.series), ("language", &query.languages)] {
        if !selected.is_empty() {
            conditions.push(format!("b.{column} IN ({})", placeholders(selected.len())));
            values.extend(selected.iter().cloned().map(Value::Text));
        }
    }
    if !query.formats.is_empty() {
        conditions.push(format!("EXISTS(SELECT 1 FROM book_files available WHERE available.book_id=b.id AND available.format IN ({}))", placeholders(query.formats.len())));
        values.extend(
            query
                .formats
                .iter()
                .map(|format| Value::Text(format.as_str().into())),
        );
    }
    for (column, selected) in [
        (
            "read_status",
            query.read_status.map(|status| status.as_str()),
        ),
        (
            "metadata_status",
            query.metadata_status.map(|status| status.as_str()),
        ),
    ] {
        if let Some(value) = selected {
            conditions.push(format!("b.{column}=?"));
            values.push(Value::Text(value.into()));
        }
    }
    if let Some(favorite) = query.favorite {
        conditions.push("b.favorite=?".into());
        values.push(Value::Integer(i64::from(favorite)));
    }
    if let Some(missing) = query.missing_cover {
        let expression = "(b.cover_relative_path IS NULL OR b.cover_relative_path='')";
        conditions.push(if missing {
            expression.into()
        } else {
            format!("NOT {expression}")
        });
    }
    for (operator, bound) in [(">=", query.min_size_bytes), ("<=", query.max_size_bytes)] {
        if let Some(bound) = bound {
            conditions.push(format!("COALESCE(selected_file.size_bytes,0){operator}?"));
            values.push(Value::Integer(bound as i64));
        }
    }
    if query.device_id.is_some() || query.on_device.is_some() {
        let selected_devices: Vec<&String> = connected_devices
            .iter()
            .filter(|id| {
                query
                    .device_id
                    .as_ref()
                    .is_none_or(|selected| *id == selected)
            })
            .collect();
        let presence = if selected_devices.is_empty() {
            "0".into()
        } else {
            values.extend(selected_devices.iter().map(|id| Value::Text((*id).clone())));
            format!(
                "EXISTS(SELECT 1 FROM device_books presence WHERE presence.book_id=b.id AND presence.device_id IN ({}))",
                placeholders(selected_devices.len())
            )
        };
        conditions.push(if query.on_device == Some(false) {
            format!("NOT ({presence})")
        } else {
            presence
        });
    }
    if conditions.is_empty() {
        ("1".into(), values)
    } else {
        (conditions.join(" AND "), values)
    }
}

fn sort_expression(sort: BookSort, descending: bool) -> String {
    let column = match sort {
        BookSort::Title => "b.title COLLATE NOCASE",
        BookSort::Author => "b.author_sort COLLATE NOCASE",
        BookSort::Series => "b.series COLLATE NOCASE",
        BookSort::Added => "b.added_at",
        BookSort::Updated => "b.updated_at",
        BookSort::Size => "COALESCE(selected_file.size_bytes,0)",
        BookSort::Progress => "b.reading_progress",
        BookSort::Published => "b.published",
        BookSort::Rating => "b.rating",
    };
    let direction = if descending { "DESC" } else { "ASC" };
    let series_index = if sort == BookSort::Series {
        format!(", b.series_index {direction} NULLS LAST")
    } else {
        String::new()
    };
    format!("{column} {direction} NULLS LAST{series_index}, b.title COLLATE NOCASE ASC, b.id ASC")
}

fn literal_search(value: &str) -> Option<String> {
    let normalized: String = value.nfkc().collect();
    let terms: Vec<String> = normalized
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .take(64)
        .map(|term| format!("\"{term}\"*"))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" AND "))
    }
}

fn literal_like(value: &str) -> String {
    let mut pattern = String::from("%");
    for character in value.trim().nfkc() {
        if matches!(character, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

fn json_facets(connection: &Connection, column: &str) -> Result<Vec<Facet>> {
    query_facets(
        connection,
        &format!(
            "SELECT item.value, count(DISTINCT b.id) FROM books b, json_each(b.{column}) item
        WHERE item.type='text' AND trim(item.value)<>'' GROUP BY item.value"
        ),
        &[],
    )
}

fn scalar_facets(connection: &Connection, column: &str) -> Result<Vec<Facet>> {
    query_facets(
        connection,
        &format!(
            "SELECT {column}, count(*) FROM books WHERE {column} IS NOT NULL AND trim({column})<>'' GROUP BY {column}"
        ),
        &[],
    )
}

fn query_facets(connection: &Connection, sql: &str, values: &[Value]) -> Result<Vec<Facet>> {
    let mut facets = connection
        .prepare(sql)?
        .query_map(params_from_iter(values.iter()), |row| {
            Ok(Facet {
                value: row.get(0)?,
                count: checked_unsigned(row.get(1)?, 1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    facets.sort_by_cached_key(|facet| (fold(&facet.value), facet.value.clone()));
    Ok(facets)
}

fn fold(value: &str) -> String {
    value
        .nfd()
        .filter(|character| !is_combining_mark(*character))
        .flat_map(char::to_lowercase)
        .collect()
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn effective_author_sort(author_sort: &str, authors: &[String]) -> String {
    if author_sort.trim().is_empty() {
        authors.join(" & ")
    } else {
        author_sort.into()
    }
}

fn validate_query(query: &BookQuery, devices: &[String]) -> Result<()> {
    validate_devices(devices)?;
    if let Some(id) = &query.device_id {
        validate_id(id)?;
    }
    if query.formats.len() > MAX_FILTER_VALUES {
        return Err(AppError::InvalidInput("Too many format filters".into()));
    }
    if query.search.len() > 4096 {
        return Err(AppError::InvalidInput("Search text is too large".into()));
    }
    for selected in [
        &query.authors,
        &query.series,
        &query.genres,
        &query.tags,
        &query.languages,
    ] {
        if selected.len() > MAX_FILTER_VALUES || selected.iter().any(|value| value.len() > 4096) {
            return Err(AppError::InvalidInput(
                "Too many or oversized filter values".into(),
            ));
        }
    }
    to_sql_integer(query.offset, "page offset")?;
    for bound in [query.min_size_bytes, query.max_size_bytes]
        .into_iter()
        .flatten()
    {
        to_sql_integer(bound, "file size filter")?;
    }
    if query
        .min_size_bytes
        .zip(query.max_size_bytes)
        .is_some_and(|(minimum, maximum)| minimum > maximum)
    {
        return Err(AppError::InvalidInput(
            "Minimum size exceeds maximum size".into(),
        ));
    }
    Ok(())
}

fn validate_devices(devices: &[String]) -> Result<()> {
    if devices.len() > MAX_CONNECTED_DEVICES {
        return Err(AppError::InvalidInput("Too many connected devices".into()));
    }
    for id in devices {
        validate_id(id)?;
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<()> {
    if id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        Err(AppError::InvalidInput("Invalid internal identifier".into()))
    } else {
        Ok(())
    }
}

fn validate_metadata(metadata: &BookMetadata) -> Result<()> {
    if metadata.title.trim().is_empty()
        || metadata.title.len() > 4096
        || metadata.title.contains('\0')
    {
        return Err(AppError::InvalidInput("A book needs a valid title".into()));
    }
    if metadata
        .series_index
        .is_some_and(|index| !index.is_finite() || index < 0.0)
    {
        return Err(AppError::InvalidInput("Invalid series index".into()));
    }
    if metadata.description.len() > 1_048_576
        || metadata.description.contains('\0')
        || metadata.language.trim().is_empty()
        || metadata.language.len() > 64
        || metadata.author_sort.len() > 4096
        || metadata.author_sort.contains('\0')
        || [
            &metadata.series,
            &metadata.isbn,
            &metadata.publisher,
            &metadata.published,
        ]
        .into_iter()
        .flatten()
        .any(|value| value.len() > 4096 || value.contains('\0'))
    {
        return Err(AppError::InvalidInput(
            "Oversized or invalid bibliographic text".into(),
        ));
    }
    for values in [&metadata.authors, &metadata.genres, &metadata.tags] {
        if values.len() > 512
            || values
                .iter()
                .any(|value| value.len() > 4096 || value.contains('\0'))
        {
            return Err(AppError::InvalidInput(
                "Too many or invalid metadata values".into(),
            ));
        }
    }
    Ok(())
}

fn validate_book(book: &Book) -> Result<()> {
    validate_id(&book.id)?;
    validate_devices(&book.on_device_ids)?;
    validate_metadata(&BookMetadata {
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
    })?;
    validate_range(Some(book.reading_progress), 0.0, 1.0, "reading progress")?;
    validate_range(book.metadata_confidence, 0.0, 1.0, "metadata confidence")?;
    validate_range(book.rating, 0.0, 5.0, "rating")?;
    if let Some(path) = &book.cover_path {
        validate_relative_path(path)?;
    }
    Ok(())
}

fn validate_range(value: Option<f64>, minimum: f64, maximum: f64, name: &str) -> Result<()> {
    if value.is_some_and(|value| !value.is_finite() || value < minimum || value > maximum) {
        Err(AppError::InvalidInput(format!("Invalid {name}")))
    } else {
        Ok(())
    }
}

fn validate_file(stored: &StoredFile) -> Result<()> {
    validate_id(&stored.file.id)?;
    validate_id(&stored.file.book_id)?;
    validate_relative_path(&stored.relative_path)?;
    validate_hash(&stored.file.sha256)?;
    to_sql_integer(stored.file.size_bytes, "file size")?;
    Ok(())
}

fn validate_relative_path(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 4096
        || value.contains(['\\', '\0'])
        || value
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || Path::new(value)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        Err(AppError::InvalidInput(
            "A stored path must remain relative to the library".into(),
        ))
    } else {
        Ok(())
    }
}

fn validate_hash(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Err(AppError::InvalidInput("Invalid SHA-256".into()))
    } else {
        Ok(())
    }
}

fn to_sql_integer(value: u64, name: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| AppError::InvalidInput(format!("{name} exceeds SQLite integer range")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{BookFormat, FileVariant};

    fn fixture() -> Result<(tempfile::TempDir, Database, BookRepository)> {
        let temporary = tempfile::tempdir()?;
        let database = Database::new(temporary.path())?;
        Ok((temporary, database.clone(), BookRepository::new(database)))
    }

    fn metadata(
        title: &str,
        authors: &[&str],
        series: Option<(&str, f64)>,
        genres: &[&str],
        language: &str,
    ) -> BookMetadata {
        BookMetadata {
            title: title.into(),
            authors: authors.iter().map(|value| (*value).into()).collect(),
            series: series.map(|value| value.0.into()),
            series_index: series.map(|value| value.1),
            genres: genres.iter().map(|value| (*value).into()).collect(),
            language: language.into(),
            ..BookMetadata::default()
        }
    }

    fn file(
        id: &str,
        book: &str,
        hash_character: char,
        format: BookFormat,
        variant: FileVariant,
        size: u64,
    ) -> StoredFile {
        StoredFile {
            file: BookFile {
                id: id.into(),
                book_id: book.into(),
                sha256: hash_character.to_string().repeat(64),
                format,
                variant,
                size_bytes: size,
                ..BookFile::default()
            },
            relative_path: format!("books/{id}.{}", format.as_str()),
        }
    }

    const REMOVAL_REQUEST: &str = "2e03a3c2-6a88-4dfe-9b0d-a4bab3a4e105";

    fn seed_device_link(database: &Database, book_id: &str) -> Result<()> {
        database.connect()?.execute(
            "INSERT INTO devices(id,label,transport) VALUES('reader','Reader','usb')",
            [],
        )?;
        database.connect()?.execute("INSERT INTO device_books(device_id,relative_path,book_id,sha256,title,format,size_bytes) VALUES('reader','Books/story.epub',?1,?2,'Device edition','epub',120)", params![book_id, "a".repeat(64)])?;
        Ok(())
    }

    #[test]
    fn catalogue_removal_and_undo_preserve_files_reading_preferences_links_and_proofs() -> Result<()>
    {
        let (temporary, database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Read story", &["Writer"], None, &[], "fr"),
            &[file(
                "original",
                "book",
                'a',
                BookFormat::Epub,
                FileVariant::Original,
                120,
            )],
            None,
        )?;
        repository.save_progress(&book.id, "chapter:7:position:19", 0.4)?;
        let mut personal = repository.get("book", &[])?;
        personal.favorite = true;
        personal.rating = Some(4.5);
        personal.notes = "Private notes".into();
        personal.metadata_status = MetadataStatus::NeedsReview;
        let before =
            repository.update_audited(&personal, personal.revision, None, "personalUpdate")?;
        seed_device_link(&database, &book.id)?;
        let proof = seed_review_job(&database, "review", &before, false)?;
        let stored = repository.files(&book.id)?;
        let physical = temporary.path().join(&stored[0].relative_path);
        std::fs::create_dir_all(physical.parent().unwrap())?;
        std::fs::write(&physical, b"Unchanged bytes outside SQL")?;
        let operations =
            repository.remove_audited(REMOVAL_REQUEST, &[(book.id.clone(), before.revision)])?;
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].kind, "catalogueRemove");
        assert!(operations[0].reversible);
        assert_eq!(
            repository
                .list(&BookQuery::default(), &["reader".into()])?
                .total,
            0
        );
        assert!(matches!(
            repository.get(&book.id, &[]),
            Err(AppError::NotFound(_))
        ));
        assert_eq!(
            repository.removal_files_for_undo(&operations[0].id)?,
            Some(stored.clone())
        );
        assert_eq!(std::fs::read(&physical)?, b"Unchanged bytes outside SQL");
        assert_eq!(review_job_result(&database, "review")?, proof);
        let detached: Option<String> = database.connect()?.query_row(
            "SELECT book_id FROM device_books WHERE device_id='reader'",
            [],
            |row| row.get(0),
        )?;
        assert!(detached.is_none());
        drop(repository);
        let reopened = BookRepository::new(Database::new(temporary.path())?);
        reopened.undo_audited(&operations[0].id)?;
        let restored = reopened.get(&book.id, &["reader".into()])?;
        assert!(restored.revision > before.revision);
        assert_eq!(restored.added_at, before.added_at);
        assert_eq!(restored.title, before.title);
        assert_eq!(restored.favorite, before.favorite);
        assert_eq!(restored.rating, before.rating);
        assert_eq!(restored.notes, before.notes);
        assert_eq!(restored.reading_progress, before.reading_progress);
        assert_eq!(restored.read_status, before.read_status);
        assert_eq!(restored.on_device_ids, ["reader"]);
        assert_eq!(
            reopened.reader_location(&book.id)?,
            Some("chapter:7:position:19".into())
        );
        assert_eq!(reopened.files(&book.id)?, stored);
        let rebased = review_job_result(&database, "review")?;
        assert_eq!(rebased["proposal"], proof["proposal"]);
        assert_eq!(rebased["review"]["state"], "pending");
        assert_eq!(rebased["review"]["sourceRevision"], before.revision);
        assert_eq!(rebased["review"]["reviewRevision"], restored.revision);
        assert_eq!(
            reopened
                .list(
                    &BookQuery {
                        search: "Read story".into(),
                        ..Default::default()
                    },
                    &[]
                )?
                .total,
            1
        );
        let twice = reopened.undo_audited(&operations[0].id)?;
        assert_eq!(twice.status, OperationStatus::Reverted);
        assert_eq!(reopened.get(&book.id, &[])?.revision, restored.revision);
        assert!(matches!(
            reopened.remove_audited(REMOVAL_REQUEST, &[(book.id, before.revision)]),
            Err(AppError::Conflict(_))
        ));
        Ok(())
    }

    #[test]
    fn catalogue_removal_batch_is_atomic_and_request_receipt_is_idempotent() -> Result<()> {
        let (_temporary, database, repository) = fixture()?;
        let first =
            repository.insert("first", metadata("First", &[], None, &[], "en"), &[], None)?;
        let second = repository.insert(
            "second",
            metadata("Second", &[], None, &[], "en"),
            &[],
            None,
        )?;
        assert!(matches!(
            repository.remove_audited(
                REMOVAL_REQUEST,
                &[
                    (first.id.clone(), first.revision),
                    (second.id.clone(), second.revision + 1)
                ]
            ),
            Err(AppError::RevisionConflict)
        ));
        assert_eq!(repository.list(&BookQuery::default(), &[])?.total, 2);
        assert!(repository.operations()?.is_empty());
        let batch = vec![
            (first.id.clone(), first.revision),
            (second.id.clone(), second.revision),
        ];
        let operations = repository.remove_audited(REMOVAL_REQUEST, &batch)?;
        let replay = BookRepository::new(database).remove_audited(
            REMOVAL_REQUEST,
            &batch.into_iter().rev().collect::<Vec<_>>(),
        )?;
        assert_eq!(replay, operations);
        assert_eq!(repository.operations()?.len(), 2);
        assert!(matches!(
            repository.remove_audited(REMOVAL_REQUEST, &[(first.id.clone(), first.revision)]),
            Err(AppError::Conflict(_))
        ));
        assert!(matches!(
            repository.remove_audited(
                REMOVAL_REQUEST,
                &[
                    (first.id.clone(), first.revision + 1),
                    (second.id, second.revision)
                ]
            ),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(repository.list(&BookQuery::default(), &[])?.total, 0);
        Ok(())
    }

    #[test]
    fn catalogue_removal_refuses_active_jobs_and_invalid_batches() -> Result<()> {
        let (_temporary, database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Busy book", &[], None, &[], "en"),
            &[],
            None,
        )?;
        for status in [
            "queued",
            "running",
            "waitingForNetwork",
            "waitingForConfiguration",
        ] {
            database.connect()?.execute(
                "INSERT INTO jobs(id,kind,status,payload_json) VALUES(?1,'chat',?2,?3)",
                params![
                    status,
                    status,
                    serde_json::json!({"version":1,"payload":{"id":"book"},"bookIds":["book"]})
                        .to_string()
                ],
            )?;
            assert!(matches!(
                repository.remove_audited(REMOVAL_REQUEST, &[(book.id.clone(), book.revision)]),
                Err(AppError::Conflict(_))
            ));
            database
                .connect()?
                .execute("DELETE FROM jobs WHERE id=?", [status])?;
        }
        for invalid in [
            vec![],
            vec![("book".into(), 0)],
            vec![("book".into(), 1), ("book".into(), 1)],
            (0..201).map(|index| (format!("book-{index}"), 1)).collect(),
        ] {
            assert!(matches!(
                repository.remove_audited(REMOVAL_REQUEST, &invalid),
                Err(AppError::InvalidInput(_))
            ));
        }
        assert!(matches!(
            repository.remove_audited("not-a-uuid", &[(book.id.clone(), book.revision)]),
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            repository.remove_audited(REMOVAL_REQUEST, &[("missing".into(), 1)]),
            Err(AppError::NotFound(_))
        ));
        assert_eq!(repository.get(&book.id, &[])?, book);
        assert!(repository.operations()?.is_empty());
        Ok(())
    }

    #[test]
    fn catalogue_removal_undo_refuses_reused_identity_files_and_changed_device_inventory()
    -> Result<()> {
        for collision in ["book", "hash", "path", "fileId", "device"] {
            let (_temporary, database, repository) = fixture()?;
            let stored = file(
                "original",
                "book",
                'a',
                BookFormat::Epub,
                FileVariant::Original,
                120,
            );
            let book = repository.insert(
                "book",
                metadata("Removed book", &[], None, &[], "en"),
                std::slice::from_ref(&stored),
                None,
            )?;
            seed_device_link(&database, &book.id)?;
            let operation = repository
                .remove_audited(REMOVAL_REQUEST, &[(book.id.clone(), book.revision)])?
                .remove(0);
            if collision == "device" {
                database.connect()?.execute(
                    "UPDATE device_books SET sha256=? WHERE device_id='reader'",
                    ["b".repeat(64)],
                )?;
            } else {
                let mut other = file(
                    "other-file",
                    "other",
                    'b',
                    BookFormat::Epub,
                    FileVariant::Original,
                    100,
                );
                match collision {
                    "hash" => other.file.sha256 = stored.file.sha256.clone(),
                    "path" => other.relative_path = stored.relative_path.clone(),
                    "fileId" => other.file.id = stored.file.id.clone(),
                    _ => {}
                }
                repository.insert(
                    if collision == "book" { "book" } else { "other" },
                    metadata("New owner", &[], None, &[], "en"),
                    if collision == "book" {
                        &[]
                    } else {
                        std::slice::from_ref(&other)
                    },
                    None,
                )?;
            }
            assert!(
                matches!(
                    repository.undo_audited(&operation.id),
                    Err(AppError::Conflict(_))
                ),
                "{collision}"
            );
            assert_eq!(
                repository
                    .operations()?
                    .iter()
                    .find(|candidate| candidate.id == operation.id)
                    .unwrap()
                    .status,
                OperationStatus::Applied
            );
        }
        Ok(())
    }

    fn seed_review_job(
        database: &Database,
        id: &str,
        book: &Book,
        legacy: bool,
    ) -> Result<serde_json::Value> {
        let proposal = serde_json::json!({"bookId":book.id,"patch":{"title":"Proposed title"},"confidence":0.8,"evidence":[{"field":"title","value":"Proposed title","confidence":0.8,"sourceUrls":[]}],"warnings":["Preserved warning"],"providerId":"minimax","modelId":"fixture"});
        let result = if legacy {
            proposal
        } else {
            serde_json::json!({"proposal":proposal,"review":{"state":"pending","sourceRevision":book.revision,"reviewRevision":book.revision}})
        };
        database.connect()?.execute("INSERT INTO jobs(id,kind,status,progress,payload_json,result_json,created_at,updated_at) VALUES(?1,'enrich','completed',1,?2,?3,?4,?4)",params![id,serde_json::json!({"version":1,"payload":{"id":book.id,"bookIds":[book.id]}}).to_string(),result.to_string(),now()])?;
        Ok(result)
    }

    fn review_job_result(database: &Database, id: &str) -> Result<serde_json::Value> {
        let raw: String = database.connect()?.query_row(
            "SELECT result_json FROM jobs WHERE id=?",
            [id],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&raw)?)
    }

    #[test]
    fn bibliographic_save_resolves_historical_reviews_and_survives_reopen() -> Result<()> {
        let (temporary, database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Original", &["Author"], None, &[], "fr"),
            &[],
            None,
        )?;
        let legacy = seed_review_job(&database, "legacy-job", &book, true)?;
        seed_review_job(&database, "pending-job", &book, false)?;
        let other =
            repository.insert("other", metadata("Other", &[], None, &[], "fr"), &[], None)?;
        let other_result = seed_review_job(&database, "other-job", &other, false)?;
        let mut edited = book.clone();
        edited.title = "Human title".into();
        edited.metadata_status = MetadataStatus::Verified;
        let updated = repository.update_audited(&edited, book.revision, None, "metadataUpdate")?;
        assert_eq!(
            review_job_result(&database, "legacy-job")?["proposal"],
            legacy
        );
        for id in ["legacy-job", "pending-job"] {
            let result = review_job_result(&database, id)?;
            assert_eq!(result["review"]["state"], "dismissed");
            assert_eq!(result["review"]["resolvedRevision"], updated.revision);
        }
        assert!(
            review_job_result(&database, "legacy-job")?["review"]
                .get("sourceRevision")
                .is_none()
        );
        assert_eq!(review_job_result(&database, "other-job")?, other_result);
        drop(repository);
        drop(database);
        let reopened = Database::new(temporary.path())?;
        assert_eq!(
            review_job_result(&reopened, "legacy-job")?["review"]["state"],
            "dismissed"
        );
        Ok(())
    }

    #[test]
    fn explicit_review_validation_can_resolve_without_metadata_changes_and_is_idempotent()
    -> Result<()> {
        let (_temporary, database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Original", &[], None, &[], "fr"),
            &[],
            None,
        )?;
        seed_review_job(&database, "review-job", &book, true)?;
        let mut validated = book.clone();
        validated.metadata_status = MetadataStatus::Verified;
        let updated = repository.update_audited_with_review(
            &validated,
            book.revision,
            None,
            "metadataUpdate",
            Some("review-job"),
        )?;
        assert_eq!(updated.title, book.title);
        assert_eq!(
            review_job_result(&database, "review-job")?["review"]["state"],
            "applied"
        );
        let duplicate = repository.update_audited_with_review(
            &updated,
            updated.revision,
            None,
            "metadataUpdate",
            Some("review-job"),
        )?;
        assert_eq!(duplicate.revision, updated.revision);
        assert_eq!(repository.operations()?.len(), 1);
        Ok(())
    }

    #[test]
    fn personal_preferences_do_not_consume_bibliographic_reviews() -> Result<()> {
        let (_temporary, database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Original", &[], None, &[], "fr"),
            &[],
            None,
        )?;
        seed_review_job(&database, "review-job", &book, false)?;
        let mut edited = book.clone();
        edited.notes = "Personal notes".into();
        edited.favorite = true;
        let updated = repository.update_audited(&edited, book.revision, None, "personalUpdate")?;
        assert_eq!(
            review_job_result(&database, "review-job")?["review"]["state"],
            "pending"
        );
        let result = review_job_result(&database, "review-job")?;
        assert_eq!(result["review"]["sourceRevision"], book.revision);
        assert_eq!(result["review"]["reviewRevision"], updated.revision);
        Ok(())
    }

    #[test]
    fn reading_progress_rebases_pending_review_without_consuming_or_fabricating_provenance()
    -> Result<()> {
        let (_temporary, database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Original", &[], None, &[], "fr"),
            &[],
            None,
        )?;
        seed_review_job(&database, "pending-job", &book, false)?;
        let legacy = seed_review_job(&database, "legacy-job", &book, true)?;
        repository.save_progress("book", "chapter:2", 0.5)?;
        let current = repository.get("book", &[])?;
        let result = review_job_result(&database, "pending-job")?;
        assert_eq!(result["review"]["state"], "pending");
        assert_eq!(result["review"]["sourceRevision"], book.revision);
        assert_eq!(result["review"]["reviewRevision"], current.revision);
        assert_eq!(review_job_result(&database, "legacy-job")?, legacy);
        repository.save_progress("book", "chapter:2", 0.5)?;
        assert_eq!(repository.get("book", &[])?.revision, current.revision);
        let mut validated = current.clone();
        validated.metadata_status = MetadataStatus::Verified;
        repository.update_audited_with_review(
            &validated,
            current.revision,
            None,
            "metadataUpdate",
            Some("pending-job"),
        )?;
        assert_eq!(
            review_job_result(&database, "pending-job")?["review"]["state"],
            "applied"
        );
        Ok(())
    }

    #[test]
    fn metadata_review_undo_restores_pending_with_fresh_cas_and_preserves_sources() -> Result<()> {
        let (_temporary, database, repository) = fixture()?;
        let mut book = repository.insert(
            "book",
            metadata("Original", &[], None, &[], "fr"),
            &[],
            None,
        )?;
        book.metadata_status = MetadataStatus::NeedsReview;
        book = repository.update_book(&book, book.revision)?;
        let original = seed_review_job(&database, "review-job", &book, false)?;
        let mut edited = book.clone();
        edited.title = "Proposed title".into();
        edited.metadata_status = MetadataStatus::Verified;
        repository.update_audited_with_review(
            &edited,
            book.revision,
            None,
            "metadataUpdate",
            Some("review-job"),
        )?;
        let operation = repository.operations()?.remove(0);
        repository.undo_audited(&operation.id)?;
        let restored = repository.get("book", &[])?;
        assert_eq!(restored.title, book.title);
        assert_eq!(restored.metadata_status, MetadataStatus::NeedsReview);
        let result = review_job_result(&database, "review-job")?;
        assert_eq!(result["proposal"], original["proposal"]);
        assert_eq!(result["review"]["state"], "pending");
        assert_eq!(result["review"]["sourceRevision"], book.revision);
        assert_eq!(result["review"]["reviewRevision"], restored.revision);
        assert!(result["review"].get("resolvedRevision").is_none());
        repository.undo_audited(&operation.id)?;
        assert_eq!(repository.get("book", &[])?.revision, restored.revision);
        edited = restored.clone();
        edited.title = "Proposed title".into();
        edited.metadata_status = MetadataStatus::Verified;
        repository.update_audited_with_review(
            &edited,
            restored.revision,
            None,
            "metadataUpdate",
            Some("review-job"),
        )?;
        Ok(())
    }

    #[test]
    fn review_undo_rejects_changed_evidence_before_restoring_book() -> Result<()> {
        let (_temporary, database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Original", &[], None, &[], "fr"),
            &[],
            None,
        )?;
        seed_review_job(&database, "review-job", &book, true)?;
        let mut edited = book.clone();
        edited.title = "Proposed title".into();
        edited.metadata_status = MetadataStatus::Verified;
        let updated = repository.update_audited_with_review(
            &edited,
            book.revision,
            None,
            "metadataUpdate",
            Some("review-job"),
        )?;
        let operation = repository.operations()?.remove(0);
        let mut result = review_job_result(&database, "review-job")?;
        result["proposal"]["evidence"][0]["value"] = JsonValue::from("Changed evidence");
        database.connect()?.execute(
            "UPDATE jobs SET result_json=? WHERE id='review-job'",
            [result.to_string()],
        )?;
        assert!(matches!(
            repository.undo_audited(&operation.id),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(repository.get("book", &[])?, updated);
        assert_eq!(repository.operations()?[0].status, OperationStatus::Applied);
        Ok(())
    }

    #[test]
    fn explicit_review_rejects_another_book_or_stale_source_and_keeps_old_choices_closed()
    -> Result<()> {
        let (_temporary, database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Original", &[], None, &[], "fr"),
            &[],
            None,
        )?;
        let other =
            repository.insert("other", metadata("Other", &[], None, &[], "fr"), &[], None)?;
        seed_review_job(&database, "other-job", &other, false)?;
        seed_review_job(&database, "review-job", &book, false)?;
        assert!(matches!(
            repository.update_audited_with_review(
                &book,
                book.revision,
                None,
                "metadataUpdate",
                Some("other-job")
            ),
            Err(AppError::NotFound(_))
        ));
        let mut changed = book.clone();
        changed.title = "Human title".into();
        let current = repository.update_book(&changed, book.revision)?;
        assert!(matches!(
            repository.update_audited_with_review(
                &current,
                current.revision,
                None,
                "metadataUpdate",
                Some("review-job")
            ),
            Err(AppError::RevisionConflict)
        ));
        changed = current.clone();
        changed.title = "Another human title".into();
        let current =
            repository.update_audited(&changed, current.revision, None, "metadataUpdate")?;
        assert_eq!(
            review_job_result(&database, "review-job")?["review"]["state"],
            "obsolete"
        );
        assert!(matches!(
            repository.update_audited_with_review(
                &current,
                current.revision,
                None,
                "metadataUpdate",
                Some("review-job")
            ),
            Err(AppError::Conflict(_))
        ));
        Ok(())
    }

    #[test]
    fn review_resolution_rolls_back_with_stale_revision_or_invalid_variant() -> Result<()> {
        let (_temporary, database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Original", &[], None, &[], "fr"),
            &[],
            None,
        )?;
        let original = seed_review_job(&database, "review-job", &book, false)?;
        let mut edited = book.clone();
        edited.title = "Changed".into();
        edited.metadata_status = MetadataStatus::Verified;
        assert!(matches!(
            repository.update_audited_with_review(
                &edited,
                book.revision + 1,
                None,
                "metadataUpdate",
                Some("review-job")
            ),
            Err(AppError::RevisionConflict)
        ));
        let invalid = file(
            "wrong-variant",
            "other",
            'a',
            BookFormat::Epub,
            FileVariant::Normalized,
            100,
        );
        assert!(
            repository
                .update_audited_with_review(
                    &edited,
                    book.revision,
                    Some(invalid),
                    "metadataUpdate",
                    Some("review-job")
                )
                .is_err()
        );
        assert_eq!(repository.get("book", &[])?, book);
        assert_eq!(review_job_result(&database, "review-job")?, original);
        assert!(repository.operations()?.is_empty());
        Ok(())
    }

    fn reviewed_relocation_fixture() -> Result<(
        tempfile::TempDir,
        Database,
        BookRepository,
        Book,
        Vec<StoredFile>,
    )> {
        let (temporary, database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Title", &["Author"], None, &[], "en"),
            &[file(
                "variant",
                "book",
                'b',
                BookFormat::Mobi,
                FileVariant::Converted,
                120,
            )],
            None,
        )?;
        seed_review_job(&database, "pending-job", &book, false)?;
        let mut relocated = repository.files(&book.id)?;
        relocated[0].relative_path = "books/Author/Title - organized.mobi".into();
        Ok((temporary, database, repository, book, relocated))
    }

    #[test]
    fn transaction_helpers_roll_back_book_review_and_audit_with_their_caller() -> Result<()> {
        let (_temporary, database, repository, book, _) = reviewed_relocation_fixture()?;
        let original = review_job_result(&database, "pending-job")?;
        let mut connection = database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status = repository.set_metadata_status_in_transaction(
            &transaction,
            &book.id,
            MetadataStatus::NeedsReview,
            Some(0.8),
            book.revision,
        )?;
        assert_eq!(status.revision, book.revision + 1);
        let stored: String = transaction.query_row(
            "SELECT result_json FROM jobs WHERE id='pending-job'",
            [],
            |row| row.get(0),
        )?;
        let result: JsonValue = serde_json::from_str(&stored)?;
        assert_eq!(result["review"]["reviewRevision"], status.revision);
        let mut edited = status;
        edited.notes = "Personal note within the publication transaction".into();
        let updated = repository.update_audited_in_transaction(
            &transaction,
            &edited,
            edited.revision,
            None,
            "personalUpdate",
            None,
        )?;
        assert_eq!(updated.revision, book.revision + 2);
        transaction.rollback()?;
        assert_eq!(repository.get(&book.id, &[])?, book);
        assert_eq!(review_job_result(&database, "pending-job")?, original);
        assert!(repository.operations()?.is_empty());
        assert!(
            repository
                .set_metadata_status_in_transaction(
                    &connection,
                    &book.id,
                    MetadataStatus::NeedsReview,
                    Some(0.8),
                    book.revision,
                )
                .is_err()
        );
        assert!(
            repository
                .update_audited_in_transaction(
                    &connection,
                    &book,
                    book.revision,
                    None,
                    "personalUpdate",
                    None,
                )
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn status_publication_reanchors_existing_reviews_and_returns_actual_revision() -> Result<()> {
        let (_temporary, database, repository, book, _) = reviewed_relocation_fixture()?;
        let legacy = seed_review_job(&database, "legacy-job", &book, true)?;
        let mut connection = database.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = repository.set_metadata_status_in_transaction(
            &transaction,
            &book.id,
            MetadataStatus::NeedsReview,
            Some(0.8),
            book.revision,
        )?;
        let unchanged = repository.set_metadata_status_in_transaction(
            &transaction,
            &book.id,
            MetadataStatus::NeedsReview,
            Some(0.8),
            changed.revision,
        )?;
        assert_eq!(changed, unchanged);
        transaction.commit()?;
        assert_eq!(repository.get(&book.id, &[])?, changed);
        let result = review_job_result(&database, "pending-job")?;
        assert_eq!(result["review"]["sourceRevision"], book.revision);
        assert_eq!(result["review"]["reviewRevision"], changed.revision);
        assert_eq!(review_job_result(&database, "legacy-job")?, legacy);
        Ok(())
    }

    #[test]
    fn organization_reanchors_pending_review_and_undo_keeps_it_applicable() -> Result<()> {
        let (_temporary, database, repository, book, relocated) = reviewed_relocation_fixture()?;
        let legacy = seed_review_job(&database, "legacy-job", &book, true)?;
        let original = review_job_result(&database, "pending-job")?;
        let organized = repository.relocate_files_audited(&book, book.revision, &relocated)?;
        let result = review_job_result(&database, "pending-job")?;
        assert_eq!(result["review"]["reviewRevision"], organized.revision);
        assert_eq!(result["proposal"], original["proposal"]);
        assert_eq!(result["review"]["sourceRevision"], book.revision);
        require_pending_review(&result, organized.revision)?;
        assert_eq!(review_job_result(&database, "legacy-job")?, legacy);
        let operation = repository.operations()?.remove(0);
        repository.undo_audited(&operation.id)?;
        let restored = repository.get(&book.id, &[])?;
        let result = review_job_result(&database, "pending-job")?;
        assert_eq!(result["review"]["state"], "pending");
        assert_eq!(result["review"]["reviewRevision"], restored.revision);
        assert_eq!(result["review"]["sourceRevision"], book.revision);
        assert_eq!(review_job_result(&database, "legacy-job")?, legacy);
        let mut reviewed = restored.clone();
        reviewed.metadata_status = MetadataStatus::Verified;
        repository.update_audited_with_review(
            &reviewed,
            restored.revision,
            None,
            "metadataUpdate",
            Some("pending-job"),
        )?;
        assert_eq!(
            review_job_result(&database, "pending-job")?["review"]["state"],
            "applied"
        );
        Ok(())
    }

    #[test]
    fn organized_book_can_apply_its_existing_metadata_proposal() -> Result<()> {
        let (_temporary, database, repository, book, relocated) = reviewed_relocation_fixture()?;
        let organized = repository.relocate_files_audited(&book, book.revision, &relocated)?;
        let mut reviewed = organized.clone();
        reviewed.title = "Proposed title".into();
        reviewed.metadata_status = MetadataStatus::Verified;
        let applied = repository.update_audited_with_review(
            &reviewed,
            organized.revision,
            None,
            "metadataUpdate",
            Some("pending-job"),
        )?;
        assert_eq!(applied.title, "Proposed title");
        let result = review_job_result(&database, "pending-job")?;
        assert_eq!(result["review"]["state"], "applied");
        assert_eq!(result["review"]["sourceRevision"], book.revision);
        assert_eq!(result["review"]["resolvedRevision"], applied.revision);
        Ok(())
    }

    #[test]
    fn organization_undo_refuses_a_review_changed_after_its_commit() -> Result<()> {
        let (_temporary, database, repository, book, relocated) = reviewed_relocation_fixture()?;
        let organized = repository.relocate_files_audited(&book, book.revision, &relocated)?;
        let operation = repository.operations()?.remove(0);
        let mut result = review_job_result(&database, "pending-job")?;
        result["review"]["state"] = JsonValue::from("dismissed");
        database.connect()?.execute(
            "UPDATE jobs SET result_json=? WHERE id='pending-job'",
            [result.to_string()],
        )?;
        assert!(matches!(
            repository.undo_audited(&operation.id),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(repository.get(&book.id, &[])?, organized);
        assert_eq!(repository.files(&book.id)?, relocated);
        assert_eq!(repository.operations()?[0].status, OperationStatus::Applied);
        Ok(())
    }

    #[test]
    fn organization_review_and_paths_roll_back_if_audit_insert_fails() -> Result<()> {
        let (_temporary, database, repository, book, relocated) = reviewed_relocation_fixture()?;
        let files = repository.files(&book.id)?;
        let result = review_job_result(&database, "pending-job")?;
        database.connect()?.execute_batch("CREATE TRIGGER reject_organization BEFORE INSERT ON operations WHEN NEW.kind='organization' BEGIN SELECT RAISE(ABORT,'fixture audit failure'); END;")?;
        assert!(
            repository
                .relocate_files_audited(&book, book.revision, &relocated)
                .is_err()
        );
        assert_eq!(repository.get(&book.id, &[])?, book);
        assert_eq!(repository.files(&book.id)?, files);
        assert_eq!(review_job_result(&database, "pending-job")?, result);
        assert!(repository.operations()?.is_empty());
        Ok(())
    }

    #[test]
    fn relocation_audits_only_paths_and_undo_restores_them_idempotently() -> Result<()> {
        let (_temporary, _database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Title", &["Author"], None, &[], "en"),
            &[
                file(
                    "source",
                    "book",
                    'a',
                    BookFormat::Epub,
                    FileVariant::Original,
                    100,
                ),
                file(
                    "variant",
                    "book",
                    'b',
                    BookFormat::Mobi,
                    FileVariant::Converted,
                    120,
                ),
            ],
            None,
        )?;
        let before = repository.files(&book.id)?;
        let mut relocated = before.clone();
        relocated
            .iter_mut()
            .find(|file| file.file.id == "variant")
            .unwrap()
            .relative_path = "books/Author/_Standalone/Title - variant.mobi".into();
        let updated = repository.relocate_files_audited(&book, book.revision, &relocated)?;
        assert_eq!(updated.revision, book.revision + 1);
        assert_eq!(updated.title, book.title);
        assert_eq!(updated.notes, book.notes);
        assert_eq!(repository.files(&book.id)?, relocated);
        assert_eq!(
            repository
                .find_file_by_hash(&"b".repeat(64))?
                .unwrap()
                .file
                .id,
            "variant"
        );
        assert_eq!(repository.operations()?.len(), 1);
        assert_eq!(
            repository.relocate_files_audited(&updated, updated.revision, &relocated)?,
            updated
        );
        assert_eq!(repository.operations()?.len(), 1);
        let operation = repository.operations()?.remove(0);
        assert_eq!(operation.kind, "organization");
        assert_eq!(
            repository.undo_audited(&operation.id)?.status,
            OperationStatus::Reverted
        );
        assert_eq!(repository.files(&book.id)?, before);
        let restored = repository.get(&book.id, &[])?;
        assert_eq!(
            repository.undo_audited(&operation.id)?.status,
            OperationStatus::Reverted
        );
        assert_eq!(repository.get(&book.id, &[])?, restored);
        Ok(())
    }

    #[test]
    fn relocation_rejects_changed_identity_book_or_file_set_without_mutating_records() -> Result<()>
    {
        let (_temporary, _database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Title", &["Author"], None, &[], "en"),
            &[
                file(
                    "source",
                    "book",
                    'a',
                    BookFormat::Epub,
                    FileVariant::Original,
                    100,
                ),
                file(
                    "variant",
                    "book",
                    'b',
                    BookFormat::Mobi,
                    FileVariant::Converted,
                    120,
                ),
            ],
            None,
        )?;
        let before = repository.files(&book.id)?;
        assert!(matches!(
            repository.relocate_files_audited(&book, book.revision + 1, &before),
            Err(AppError::RevisionConflict)
        ));
        let mut edited = book.clone();
        edited.title = "Must not change".into();
        assert!(matches!(
            repository.relocate_files_audited(&edited, book.revision, &before),
            Err(AppError::InvalidInput(_))
        ));
        let variant = before
            .iter()
            .find(|file| file.file.id == "variant")
            .unwrap();
        let original = before.iter().find(|file| file.file.id == "source").unwrap();
        let changes: [fn(&mut StoredFile); 8] = [
            |file| file.file.id = "unknown".into(),
            |file| file.file.book_id = "foreign-book".into(),
            |file| file.file.sha256 = "c".repeat(64),
            |file| file.file.size_bytes += 1,
            |file| file.file.variant = FileVariant::Normalized,
            |file| file.file.format = BookFormat::Epub,
            |file| file.file.profile = Some("other-profile".into()),
            |file| file.file.created_at = "2026-01-01T00:00:00Z".into(),
        ];
        for change in changes {
            let mut altered = variant.clone();
            altered.relative_path = "books/Author/new.mobi".into();
            change(&mut altered);
            assert!(matches!(
                repository.relocate_files_audited(
                    &book,
                    book.revision,
                    &[original.clone(), altered]
                ),
                Err(AppError::InvalidInput(_))
            ));
        }
        assert!(matches!(
            repository.relocate_files_audited(&book, book.revision, std::slice::from_ref(variant)),
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            repository.relocate_files_audited(
                &book,
                book.revision,
                &[variant.clone(), variant.clone()]
            ),
            Err(AppError::InvalidInput(_))
        ));
        assert_eq!(repository.get(&book.id, &[])?, book);
        assert_eq!(repository.files(&book.id)?, before);
        assert!(repository.operations()?.is_empty());
        Ok(())
    }

    #[test]
    fn relocation_refuses_original_moves_unsafe_paths_and_collisions_atomically() -> Result<()> {
        let (_temporary, _database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Title", &["Author"], None, &[], "en"),
            &[
                file(
                    "source",
                    "book",
                    'a',
                    BookFormat::Epub,
                    FileVariant::Original,
                    100,
                ),
                file(
                    "variant",
                    "book",
                    'b',
                    BookFormat::Mobi,
                    FileVariant::Converted,
                    120,
                ),
            ],
            None,
        )?;
        repository.insert(
            "foreign",
            metadata("Other", &[], None, &[], "en"),
            &[file(
                "occupied",
                "foreign",
                'c',
                BookFormat::Mobi,
                FileVariant::Converted,
                130,
            )],
            None,
        )?;
        let before = repository.files(&book.id)?;
        let mut original_move = before.clone();
        original_move
            .iter_mut()
            .find(|file| file.file.id == "source")
            .unwrap()
            .relative_path = "books/Author/source.epub".into();
        assert!(matches!(
            repository.relocate_files_audited(&book, book.revision, &original_move),
            Err(AppError::InvalidInput(_))
        ));
        for path in [
            "/outside.mobi",
            "../outside.mobi",
            "books/../outside.mobi",
            "originals/new.mobi",
            "covers/new.mobi",
            "books/Author\\new.mobi",
            "books/Author/new\0.mobi",
        ] {
            let mut altered = before.clone();
            altered
                .iter_mut()
                .find(|file| file.file.id == "variant")
                .unwrap()
                .relative_path = path.into();
            assert!(matches!(
                repository.relocate_files_audited(&book, book.revision, &altered),
                Err(AppError::InvalidInput(_))
            ));
        }
        let mut collision = before.clone();
        collision
            .iter_mut()
            .find(|file| file.file.id == "variant")
            .unwrap()
            .relative_path = "books/occupied.mobi".into();
        assert!(matches!(
            repository.relocate_files_audited(&book, book.revision, &collision),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(repository.get(&book.id, &[])?, book);
        assert_eq!(repository.files(&book.id)?, before);
        assert!(repository.operations()?.is_empty());
        Ok(())
    }

    #[test]
    fn relocation_undo_collision_rolls_back_book_paths_and_operation_status() -> Result<()> {
        let (_temporary, _database, repository) = fixture()?;
        let book = repository.insert(
            "book",
            metadata("Title", &["Author"], None, &[], "en"),
            &[file(
                "variant",
                "book",
                'b',
                BookFormat::Mobi,
                FileVariant::Converted,
                120,
            )],
            None,
        )?;
        let mut files = repository.files(&book.id)?;
        files[0].relative_path = "books/Author/new.mobi".into();
        let updated = repository.relocate_files_audited(&book, book.revision, &files)?;
        let operation = repository.operations()?.remove(0);
        let mut occupying = file(
            "occupying",
            "foreign",
            'c',
            BookFormat::Mobi,
            FileVariant::Converted,
            130,
        );
        occupying.relative_path = "books/variant.mobi".into();
        repository.insert(
            "foreign",
            metadata("Other", &[], None, &[], "en"),
            &[occupying.clone()],
            None,
        )?;
        let occupying_before = repository.file_by_id("occupying")?;
        assert!(matches!(
            repository.undo_audited(&operation.id),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(repository.get(&book.id, &[])?, updated);
        assert_eq!(repository.files(&book.id)?, files);
        assert_eq!(repository.file_by_id("occupying")?, occupying_before);
        assert_eq!(
            repository.operations()?.remove(0).status,
            OperationStatus::Applied
        );
        Ok(())
    }

    #[test]
    fn imports_are_idempotent_and_unchanged_variants_reuse_content() -> Result<()> {
        let (_temporary, _database, repository) = fixture()?;
        let source = file(
            "source",
            "book",
            'a',
            BookFormat::Epub,
            FileVariant::Original,
            100,
        );
        let values = metadata("Épopée", &["Auteur"], None, &["Fantasy"], "fr");
        let imported = repository.insert(
            "book",
            values.clone(),
            std::slice::from_ref(&source),
            Some("covers/book.jpg"),
        )?;
        let repeated = repository.insert(
            "book",
            values,
            std::slice::from_ref(&source),
            Some("covers/book.jpg"),
        )?;
        assert_eq!(imported, repeated);
        let unchanged = file(
            "optimized",
            "book",
            'a',
            BookFormat::Epub,
            FileVariant::Optimized,
            100,
        );
        repository.add_file(unchanged)?;
        assert_eq!(repository.files("book")?.len(), 1);
        assert_eq!(repository.get("book", &[])?.revision, 1);
        assert_eq!(
            repository
                .find_original_by_hash(&"a".repeat(64))?
                .map(|book| book.id),
            Some("book".into())
        );
        assert_eq!(
            repository
                .find_original_hash(&"a".repeat(64))?
                .map(|file| file.file.id),
            Some("source".into())
        );
        assert_eq!(
            repository.file_by_id("source")?.relative_path,
            "books/source.epub"
        );
        assert_eq!(
            repository
                .find_file_by_hash(&"A".repeat(64))?
                .map(|file| file.file.id),
            Some("source".into())
        );
        let mut wrong_book = source.clone();
        wrong_book.file.book_id = "other".into();
        wrong_book.file.id = "other-file".into();
        wrong_book.relative_path = "books/other.epub".into();
        assert!(matches!(
            repository.insert(
                "other",
                metadata("Other", &[], None, &[], "en"),
                &[wrong_book],
                None
            ),
            Err(AppError::Conflict(_))
        ));
        assert!(matches!(
            repository.get("other", &[]),
            Err(AppError::NotFound(_))
        ));
        Ok(())
    }

    #[test]
    fn optimistic_updates_keep_the_search_index_in_the_same_transaction() -> Result<()> {
        let (_temporary, _database, repository) = fixture()?;
        let original = repository.insert(
            "book",
            metadata("Old title", &["Auteur"], None, &[], "fr"),
            &[],
            None,
        )?;
        let mut edited = original.clone();
        edited.title = "Nouvelle épopée".into();
        edited.favorite = true;
        edited.rating = Some(4.5);
        edited.notes = "Notes privées".into();
        let saved = repository.update_book(&edited, original.revision)?;
        assert_eq!(saved.revision, 2);
        assert!(saved.favorite);
        assert!(matches!(
            repository.update_book(&original, original.revision),
            Err(AppError::RevisionConflict)
        ));
        let refused = repository
            .update_book(&original, original.revision)
            .unwrap_err();
        assert_eq!(
            crate::models::PublicError::from(&refused).code,
            crate::models::ErrorCode::RevisionConflict
        );
        assert_eq!(repository.get("book", &[])?, saved);
        assert_eq!(
            repository
                .list(
                    &BookQuery {
                        search: "nouvelle epopee".into(),
                        ..BookQuery::default()
                    },
                    &[]
                )?
                .total,
            1
        );
        assert_eq!(
            repository
                .list(
                    &BookQuery {
                        search: "old title".into(),
                        ..BookQuery::default()
                    },
                    &[]
                )?
                .total,
            0
        );
        assert_eq!(repository.get("book", &[])?.notes, "Notes privées");
        Ok(())
    }

    #[test]
    fn filters_use_or_within_dimensions_and_and_between_dimensions() -> Result<()> {
        let (_temporary, _database, repository) = fixture()?;
        repository.insert(
            "a",
            metadata("A", &["Alice"], Some(("Saga", 3.5)), &["Fantasy"], "fr"),
            &[],
            None,
        )?;
        repository.insert(
            "b",
            metadata("B", &["Bob", "Alice"], Some(("Saga", 2.0)), &["SF"], "fr"),
            &[],
            None,
        )?;
        repository.insert(
            "c",
            metadata("C", &["Bob"], None, &["Fantasy"], "en"),
            &[],
            None,
        )?;
        let query = BookQuery {
            authors: vec!["Alice".into(), "Bob".into()],
            genres: vec!["Fantasy".into(), "SF".into()],
            languages: vec!["fr".into()],
            sort: BookSort::Series,
            descending: false,
            ..BookQuery::default()
        };
        let page = repository.list(&query, &[])?;
        assert_eq!(page.total, 2);
        assert_eq!(
            page.items
                .iter()
                .map(|book| book.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        let facets = repository.facets(&[])?;
        assert_eq!(
            facets
                .authors
                .iter()
                .find(|facet| facet.value == "Alice")
                .map(|facet| facet.count),
            Some(2)
        );
        assert_eq!(
            facets
                .genres
                .iter()
                .find(|facet| facet.value == "Fantasy")
                .map(|facet| facet.count),
            Some(2)
        );
        Ok(())
    }

    #[test]
    fn presence_is_only_computed_for_currently_connected_devices() -> Result<()> {
        let (_temporary, database, repository) = fixture()?;
        repository.insert("a", metadata("A", &[], None, &[], "fr"), &[], None)?;
        repository.insert("b", metadata("B", &[], None, &[], "fr"), &[], None)?;
        let connection = database.connect()?;
        connection.execute(
            "INSERT INTO devices(id,label,transport) VALUES('card','Card','usb')",
            [],
        )?;
        connection.execute("INSERT INTO device_books(device_id,relative_path,book_id,format,size_bytes) VALUES('card','A.epub','a','epub',10)", [])?;
        let connected = vec!["card".into()];
        assert_eq!(repository.get("a", &connected)?.on_device_ids, connected);
        let query = BookQuery {
            on_device: Some(true),
            ..BookQuery::default()
        };
        assert_eq!(repository.list(&query, &connected)?.total, 1);
        assert_eq!(repository.list(&query, &[])?.total, 0);
        assert!(repository.get("a", &[])?.on_device_ids.is_empty());
        assert_eq!(repository.facets(&connected)?.devices[0].count, 1);
        assert!(repository.facets(&[])?.devices.is_empty());
        assert_eq!(
            repository
                .list(
                    &BookQuery {
                        on_device: Some(false),
                        ..BookQuery::default()
                    },
                    &[]
                )?
                .total,
            2
        );
        assert_eq!(
            repository
                .list(
                    &BookQuery {
                        device_id: Some("card".into()),
                        ..BookQuery::default()
                    },
                    &[]
                )?
                .total,
            0
        );
        Ok(())
    }

    #[test]
    fn all_sort_modes_are_valid_and_original_files_define_the_display_format() -> Result<()> {
        let (_temporary, _database, repository) = fixture()?;
        let files = [
            file(
                "original",
                "book",
                'a',
                BookFormat::Mobi,
                FileVariant::Original,
                300,
            ),
            file(
                "converted",
                "book",
                'b',
                BookFormat::Epub,
                FileVariant::Converted,
                100,
            ),
        ];
        let book = repository.insert(
            "book",
            metadata("Book", &["Auteur"], Some(("Saga", 0.0)), &[], "fr"),
            &files,
            None,
        )?;
        assert_eq!(book.format, BookFormat::Mobi);
        assert_eq!(book.size_bytes, 300);
        for sort in [
            BookSort::Title,
            BookSort::Author,
            BookSort::Series,
            BookSort::Added,
            BookSort::Updated,
            BookSort::Size,
            BookSort::Progress,
            BookSort::Published,
            BookSort::Rating,
        ] {
            for descending in [false, true] {
                assert_eq!(
                    repository
                        .list(
                            &BookQuery {
                                sort,
                                descending,
                                limit: 1000,
                                formats: vec![BookFormat::Epub],
                                ..BookQuery::default()
                            },
                            &[]
                        )?
                        .limit,
                    200
                );
            }
        }
        assert_eq!(
            repository
                .list(
                    &BookQuery {
                        min_size_bytes: Some(200),
                        max_size_bytes: Some(400),
                        ..BookQuery::default()
                    },
                    &[]
                )?
                .total,
            1
        );
        Ok(())
    }

    #[test]
    fn progress_statuses_and_reader_location_survive_reopening() -> Result<()> {
        let (_temporary, database, repository) = fixture()?;
        repository.insert("book", metadata("Book", &[], None, &[], "fr"), &[], None)?;
        repository.save_progress("book", "section:3", 0.25)?;
        repository.set_metadata_status("book", MetadataStatus::Verified, Some(0.95))?;
        let reopened = BookRepository::new(database);
        let book = reopened.get("book", &[])?;
        assert_eq!(book.read_status, ReadStatus::Reading);
        assert_eq!(book.reading_progress, 0.25);
        assert_eq!(book.metadata_status, MetadataStatus::Verified);
        assert_eq!(reopened.reader_location("book")?, Some("section:3".into()));
        reopened.save_progress("book", "section:3", 0.25)?;
        reopened.set_metadata_status("book", MetadataStatus::Verified, Some(0.95))?;
        assert_eq!(reopened.get("book", &[])?.revision, book.revision);
        reopened.save_progress("book", "section:20", 1.0)?;
        assert_eq!(reopened.get("book", &[])?.read_status, ReadStatus::Finished);
        assert!(
            reopened
                .save_progress("book", "section:0", f64::NAN)
                .is_err()
        );
        assert!(matches!(
            reopened.save_progress("missing", "section:0", 0.0),
            Err(AppError::NotFound(_))
        ));
        Ok(())
    }

    #[test]
    fn revision_bound_review_status_preserves_human_edits_and_progress() -> Result<()> {
        let (_temporary, _database, repository) = fixture()?;
        let original =
            repository.insert("book", metadata("Book", &[], None, &[], "fr"), &[], None)?;
        let mut edited = original.clone();
        edited.title = "Human correction".into();
        edited.metadata_status = MetadataStatus::Verified;
        edited.metadata_confidence = None;
        repository.update_book(&edited, original.revision)?;
        repository.save_progress("book", "section:0", 0.3)?;
        assert!(matches!(
            repository.set_metadata_status_if_revision(
                "book",
                MetadataStatus::NeedsReview,
                Some(0.8),
                original.revision
            ),
            Err(AppError::RevisionConflict)
        ));
        let current = repository.get("book", &[])?;
        assert_eq!(current.title, "Human correction");
        assert_eq!(current.metadata_status, MetadataStatus::Verified);
        assert_eq!(current.reading_progress, 0.3);
        repository.set_metadata_status_if_revision(
            "book",
            MetadataStatus::Verified,
            None,
            current.revision,
        )?;
        assert_eq!(repository.get("book", &[])?.revision, current.revision);
        repository.set_metadata_status_if_revision(
            "book",
            MetadataStatus::NeedsReview,
            Some(0.8),
            current.revision,
        )?;
        assert_eq!(repository.get("book", &[])?.revision, current.revision + 1);
        assert_eq!(repository.get("book", &[])?.reading_progress, 0.3);
        Ok(())
    }

    #[test]
    fn hostile_search_and_storage_values_are_rejected_or_treated_literally() -> Result<()> {
        let (_temporary, _database, repository) = fixture()?;
        repository.insert(
            "book",
            metadata("Épopée", &["Auteur"], None, &[], "fr"),
            &[],
            None,
        )?;
        assert_eq!(
            repository
                .list(
                    &BookQuery {
                        search: "epopee".into(),
                        ..BookQuery::default()
                    },
                    &[]
                )?
                .total,
            1
        );
        assert_eq!(
            repository
                .list(
                    &BookQuery {
                        search: "e\u{301}pope\u{301}e".into(),
                        ..BookQuery::default()
                    },
                    &[]
                )?
                .total,
            1
        );
        for search in [
            "\" OR 1=1 --",
            "title:NEAR(\"",
            "\"",
            "'; DROP TABLE books; --",
        ] {
            assert!(
                repository
                    .list(
                        &BookQuery {
                            search: search.into(),
                            ..BookQuery::default()
                        },
                        &[]
                    )
                    .is_ok()
            );
        }
        assert_eq!(repository.list(&BookQuery::default(), &[])?.total, 1);
        let unsafe_file = StoredFile {
            relative_path: "../outside.epub".into(),
            ..file(
                "unsafe",
                "book",
                'c',
                BookFormat::Epub,
                FileVariant::Original,
                1,
            )
        };
        assert!(matches!(
            repository.add_file(unsafe_file),
            Err(AppError::InvalidInput(_))
        ));
        assert!(
            repository
                .list(
                    &BookQuery {
                        offset: u64::MAX,
                        ..BookQuery::default()
                    },
                    &[]
                )
                .is_err()
        );
        assert!(
            repository
                .list(
                    &BookQuery {
                        min_size_bytes: Some(10),
                        max_size_bytes: Some(1),
                        ..BookQuery::default()
                    },
                    &[]
                )
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn isbn_publisher_and_private_notes_search_existing_rows_without_reindexing() -> Result<()> {
        let (_temporary, database, repository) = fixture()?;
        repository.insert(
            "compact",
            metadata("Unrelated first", &["Author"], None, &[], "en"),
            &[],
            None,
        )?;
        repository.insert(
            "hyphenated",
            metadata("Unrelated second", &["Author"], None, &[], "en"),
            &[],
            None,
        )?;
        // Simulate authoritative metadata on an already indexed library from an older release.
        database.connect()?.execute("UPDATE books SET isbn='9780743273565',publisher='Vintage Press',notes='Private gift: 100%_confirmed\\detail' WHERE id='compact'",[])?;
        database.connect()?.execute("UPDATE books SET isbn='978-0-7432-7356-5',publisher='Another publisher',notes='Different private note' WHERE id='hyphenated'",[])?;
        let found = |search: &str| {
            repository.list(
                &BookQuery {
                    search: search.into(),
                    ..BookQuery::default()
                },
                &[],
            )
        };
        for search in ["9780743273565", "978-0-7432-7356-5", "978 0 7432 7356 5"] {
            assert_eq!(found(search)?.total, 2, "{search}");
        }
        for search in [
            "vintage press",
            "Private gift",
            "100%_confirmed\\detail",
            "%",
            "_",
        ] {
            let page = found(search)?;
            assert_eq!(page.total, 1, "{search}");
            assert_eq!(page.items[0].id, "compact");
        }
        assert_eq!(found("'; DROP TABLE books; --")?.total, 0);
        assert_eq!(found("% OR 1=1 --")?.total, 0);
        assert_eq!(repository.list(&BookQuery::default(), &[])?.total, 2);
        Ok(())
    }
}
