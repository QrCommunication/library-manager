#!/usr/bin/env python3
"""Offline native assistant regression checks on a fresh isolated profile.

Start tauri-driver with the same XDG_DATA_HOME as --profile-root, inside a
network-isolated namespace/container. The caller owns that isolation: a client
cannot change an already running driver's environment. Never pass a user profile.
Provider-dependent actions must be disabled without configuration. Permission
and catalogue checks temporarily use a non-persistent synthetic key; external
network isolation is mandatory and settings/keys are restored afterwards.
The catalogue fixture seeds completed results only after stopping the app,
inside the newly created synthetic profile. It never edits a user database.
"""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import uuid


spec = importlib.util.spec_from_file_location("native_smoke", Path(__file__).with_name("native-smoke.py"))
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)
Driver = native.Driver
require = native.require
checked = native.checked
REVIEW_BOOK_ID = "9c9928f9-afc6-44a2-90aa-980f1262edfb"
REVIEW_JOB_ID = "dd7b0ba9-e12d-464e-8975-6207d13f103a"
VERIFY_SELECTED = ".selected-books .selection-actions button.button.primary"
CHOOSE_BOOKS = ".selected-books .spread > button.button.ghost"


def review_profile(value):
    path = Path(value)
    require(path.is_absolute() and not path.is_symlink() and ".." not in path.parts,
            "reviewProfileMustBePrivateAbsolutePath")
    root = path.resolve(strict=True)
    require(root.is_dir() and root.is_relative_to(Path("/tmp")) and root != Path("/tmp"),
            "reviewProfileMustBePrivateTmpCopy")
    marker_path = root / ".native-assistant-fixture.json"
    require(marker_path.is_file() and not marker_path.is_symlink() and marker_path.stat().st_size <= 65536,
            "reviewFixtureMarkerMissing")
    marker = json.loads(marker_path.read_text(encoding="utf-8"))
    require(isinstance(marker, dict) and marker.get("schemaVersion") == 1
            and marker.get("purpose") == "native-assistant-review-copy"
            and marker.get("bookId") == REVIEW_BOOK_ID and marker.get("jobId") == REVIEW_JOB_ID,
            "reviewFixtureMarkerInvalid")
    require(isinstance(marker.get("originalFiles"), list) and marker["originalFiles"]
            and all(isinstance(entry, dict) and all(isinstance(entry.get(key), str)
                    for key in ("id", "relativePath", "sha256")) for entry in marker["originalFiles"]),
            "reviewOriginalFileManifestMissing")
    return root, marker


def original_hashes(root, manifest, originals):
    require({entry.get("id") for entry in manifest} == {file["id"] for file in originals},
            "reviewOriginalManifestIdentityMismatch")
    hashes = {}
    for entry in manifest:
        relative = Path(entry["relativePath"])
        require(not relative.is_absolute() and ".." not in relative.parts,
                "reviewOriginalManifestPathUnsafe")
        path = root / relative
        require(not path.is_symlink() and path.resolve(strict=True).is_relative_to(root)
                and path.is_file(), "reviewOriginalManifestPathOutsideLibrary")
        catalog = next(file for file in originals if file["id"] == entry["id"])
        digest = native.file_sha256(path)
        require(digest == entry["sha256"] == catalog["sha256"], "reviewOriginalBytesMismatch")
        hashes[entry["id"]] = digest
    return hashes


def review_smoke(driver, binary, profile, marker, report):
    with checked(report, "privateReviewFixtureAndCompletedProposal"):
        driver.start(binary)
        driver.invoke("app_bootstrap")
        settings = driver.invoke("settings_get")
        root = Path(settings["libraryRoot"]).resolve(strict=True)
        require(root.is_relative_to(profile) and root != profile, "reviewLibraryOutsidePrivateCopy")
        require(not settings["autoEnrich"] and not settings["webEnabled"]
                and settings["providerId"] is None and settings["modelId"] is None,
                "reviewFixtureMustHaveNoProviderAndAutomaticJobs")
        jobs = driver.invoke("jobs_list")
        job = next((job for job in jobs if job["id"] == REVIEW_JOB_ID), None)
        require(job is not None and job["kind"] == "enrich" and job["status"] == "completed"
                and REVIEW_BOOK_ID in job["bookIds"], "reviewCompletedJobMissing")
        require(all(job["status"] in {"completed", "failed", "cancelled"} for job in jobs),
                "reviewFixtureHasExecutableOrWaitingJobs")
        result = job["result"]
        require(isinstance(result, dict), "reviewCompletedJobResultMissing")
        proposal = result.get("proposal", result)
        require(isinstance(proposal, dict) and proposal.get("bookId") == REVIEW_BOOK_ID
                and isinstance(proposal.get("patch"), dict),
                "reviewProposalMissing")
        patch = {key: value for key, value in proposal["patch"].items()
                 if key not in {"notes", "favorite", "rating", "readStatus"}}
        before = driver.invoke("book_get", {"id": REVIEW_BOOK_ID})
        require(isinstance(patch.get("isbn"), str) and patch["isbn"] != before["isbn"],
                "reviewFixtureAlreadyAppliedOrIsbnMissing")
        before_files = driver.invoke("book_files", {"id": REVIEW_BOOK_ID})
        originals = [file for file in before_files if file["variant"] == "original"]
        require(originals, "reviewOriginalFileMissing")
        before_hashes = original_hashes(root, marker["originalFiles"], originals)
        operations_before = {operation["id"] for operation in driver.invoke("operations_list")}

    with checked(report, "persistedProposalVisibleBeforeEditor"):
        # Derive the fixture's queue from official completed results, rather
        # than the needsReview label, which can exist without a proposal.
        eligible_ids = set()
        for candidate_job in driver.invoke("jobs_list"):
            if candidate_job["kind"] != "enrich" or candidate_job["status"] != "completed":
                continue
            result = candidate_job.get("result")
            if not isinstance(result, dict):
                continue
            candidate = result.get("proposal", result)
            if not isinstance(candidate, dict) or not isinstance(candidate.get("patch"), dict) or not candidate["patch"]:
                continue
            identifier = candidate.get("bookId")
            if not isinstance(identifier, str) or identifier not in candidate_job["bookIds"]:
                continue
            current = driver.invoke("book_get", {"id": identifier})
            if current["metadataStatus"] == "needsReview":
                eligible_ids.add(identifier)
        require(eligible_ids == {REVIEW_BOOK_ID}, "reviewFixtureEligibleProposalsUnexpected")
        click(driver, ".toolbar > button.button.secondary", "globalReviewButtonMissing")
        driver.wait(lambda: (state if (state := title_field(driver)) and state["value"] == before["title"] else False), "globalReviewOpenedBookWithoutProposal")
        driver.wait(lambda: driver.execute("return !!document.querySelector('section[aria-labelledby=metadata-proposal-title]');"), "globalReviewProposalMissing")
        report["globalReview"] = {"openedEligibleProposalBook": True,
                                  "eligibleProposalCount": len(eligible_ids), "openedTarget": True}
        click(driver, ".drawer-header button", "globalReviewPanelCloseMissing")
        require(driver.execute("const node=document.querySelector('.global-search input[type=search]'); if(!node)return false; node.value=arguments[0]; node.dispatchEvent(new Event('input',{bubbles:true})); return true;", [before["title"]]), "reviewSearchInputMissing")
        driver.wait(lambda: driver.execute("return [...document.querySelectorAll('.review-action')].some(n=>n.getAttribute('aria-label')===arguments[0]);", ["Examiner : " + before["title"]]), "reviewTargetBookNotFound")
        require(driver.execute("const node=[...document.querySelectorAll('.review-action')].find(n=>n.getAttribute('aria-label')===arguments[0]); if(!node)return false; node.click(); return true;", ["Examiner : " + before["title"]]), "reviewTargetBookButtonMissing")
        driver.wait(lambda: title_field(driver), "reviewBookPanelDidNotLoad")
        presentation = driver.wait(lambda: driver.execute("const section=document.querySelector('section[aria-labelledby=metadata-proposal-title]'); const form=document.querySelector('#book-metadata-form'); const button=section?.querySelector('button.button.primary'); if(!section || !form || !button || button.disabled)return false; return {beforeForm:!!(section.compareDocumentPosition(form)&Node.DOCUMENT_POSITION_FOLLOWING),visible:section.getBoundingClientRect().height>0,isbn:!![...section.querySelectorAll('.proposal-field')].find(n=>n.textContent.includes('ISBN')&&n.textContent.includes(arguments[0])),evidence:section.querySelectorAll('.evidence').length,sources:section.querySelectorAll('.evidence .source-chip').length};", [patch["isbn"]]), "reviewProposalDidNotBecomeApplicable")
        report["proposalPresentation"] = presentation
        expected_sources = sum(len(set(evidence["sourceUrls"])) for evidence in proposal["evidence"])
        require(presentation["beforeForm"] and presentation["visible"] and presentation["isbn"]
                and presentation["evidence"] == len(proposal["evidence"])
                and presentation["sources"] == expected_sources,
                "reviewProposalOrEvidenceNotVisible")

    with checked(report, "manualNativeProposalApplicationAndOriginalPreservation"):
        click(driver, "section[aria-labelledby=metadata-proposal-title] button.button.primary", "reviewApplyButtonMissing")
        applied = driver.wait(lambda: (book if (book := driver.invoke("book_get", {"id": REVIEW_BOOK_ID}))["revision"] > before["revision"] and book["isbn"] == patch["isbn"] else False), "reviewManualApplicationDidNotPersist")
        require(all(applied.get(key) == value for key, value in patch.items()), "reviewAppliedPatchMismatch")
        driver.wait(lambda: driver.execute("return !!document.querySelector('#book-metadata-form') && !document.querySelector('section[aria-labelledby=metadata-proposal-title]');"), "appliedProposalRemainedVisible")
        report["proposalDisappearedAfterApply"] = True
        after_files = driver.invoke("book_files", {"id": REVIEW_BOOK_ID})
        require([file for file in after_files if file["variant"] == "original"] == originals,
                "reviewApplicationChangedOriginalCatalog")
        require(original_hashes(root, marker["originalFiles"], originals) == before_hashes,
                "reviewApplicationChangedOriginalBytes")
        before_ids = {file["id"] for file in before_files}
        require(any(file["variant"] == "normalized" and file["id"] not in before_ids for file in after_files),
                "reviewApplicationDidNotPublishNormalizedVariant")
        operations = [operation for operation in driver.invoke("operations_list")
                      if operation["id"] not in operations_before and operation["reversible"]]
        require(len(operations) == 1, "reviewApplicationUndoOperationMissingOrAmbiguous")
        report["manualApply"] = {"persisted": True, "revisionAdvanced": True,
                                 "patchMatched": True, "normalizedVariantCreated": True,
                                 "originalCatalogUnchanged": True, "originalBytesUnchanged": True}

    with checked(report, "officialUndoRestoresMetadataAndActiveVariants"):
        undone = driver.invoke("operation_undo", {"id": operations[0]["id"]})
        require(undone["status"] == "reverted", "reviewUndoDidNotComplete")
        restored = driver.invoke("book_get", {"id": REVIEW_BOOK_ID})
        require(restored["revision"] > applied["revision"]
                and all(restored.get(key) == before.get(key) for key in patch),
                "reviewUndoDidNotRestoreMetadata")
        restored_files = driver.invoke("book_files", {"id": REVIEW_BOOK_ID})
        require(sorted(restored_files, key=lambda file: file["id"]) == sorted(before_files, key=lambda file: file["id"]),
                "reviewUndoDidNotRestoreActiveVariants")
        require(original_hashes(root, marker["originalFiles"], originals) == before_hashes,
                "reviewUndoChangedOriginalBytes")
        report["undo"] = {"completed": True, "metadataRestored": True,
                          "activeVariantsRestored": True, "originalBytesUnchanged": True}
        report["profileMode"] = "privateReviewCopy"


STORM_SCRIPT = r"""
const done = arguments[arguments.length - 1];
const bookId = arguments[0];
const field = document.querySelector('#book-title');
if (!field) { done({error:'fieldMissing'}); return; }
const draft = field.value;
const started = performance.now();
const result = {emitted:0,emitFailures:0,disabled:0,replaced:0,draftLost:0,focusLost:0};
const calls = [];
const sample = () => {
  if (!field.isConnected || document.querySelector('#book-title') !== field) result.replaced++;
  if (field.matches(':disabled')) result.disabled++;
  if (field.value !== draft) result.draftLost++;
  if (document.activeElement !== field) result.focusLost++;
};
const onBlur = () => { result.focusLost++; };
field.addEventListener('blur', onBlur);
const observer = new MutationObserver(records => {
  for (const record of records) {
    if (record.type === 'attributes' && record.attributeName === 'disabled'
        && (record.target === field || record.target.contains(field))
        && record.oldValue === null) result.disabled++;
    for (const removed of record.removedNodes) {
      if (removed === field || removed.contains?.(field)) result.replaced++;
    }
  }
});
observer.observe(document.body, {subtree:true,childList:true,attributes:true,
  attributeFilter:['disabled'],attributeOldValue:true});
const samples = setInterval(sample, 10);
const events = setInterval(() => {
  result.emitted++;
  calls.push(window.__TAURI_INTERNALS__.invoke('plugin:event|emit', {
    event:'library:changed', payload:{bookIds:[bookId],reason:'assistantNativeSmoke'}
  }).catch(() => { result.emitFailures++; }));
}, 50);
setTimeout(async () => {
  clearInterval(events); clearInterval(samples); sample();
  await Promise.all(calls);
  observer.disconnect(); field.removeEventListener('blur', onBlur);
  result.durationMs = Math.round(performance.now() - started);
  done(result);
}, 3300);
"""


def click(driver, selector, code):
    require(driver.execute("const node=document.querySelector(arguments[0]); if(!node) return false; node.click(); return true;", [selector]), code)


def navigate(driver, assistant):
    require(driver.execute("const nodes=[...document.querySelectorAll('.nav-item')]; const node=arguments[0] ? nodes.find(n=>n.getAttribute('aria-label')==='Assistant' || n.textContent.trim()==='Assistant') : nodes[0]; if(!node)return false; node.click(); return true;", [assistant]), "navigationButtonMissing")


def library_view_state(driver):
    return driver.execute("const sort=document.querySelector('#library-sort');const read=document.querySelector('#filter-read');const page=document.querySelector('.pagination');if(!sort||!read||!page||document.querySelector('main section.page')?.getAttribute('aria-busy')==='true')return false;return {sort:sort.value,readStatus:read.value,facetsVisible:!!document.querySelector('#library-facets'),pagination:page.querySelector('span')?.textContent,orderedTitles:[...document.querySelectorAll('.book-cover')].map(n=>n.getAttribute('aria-label'))};")


def title_field(driver):
    return driver.execute("const field=document.querySelector('dialog[open] #book-title'); return field && !field.matches(':disabled') ? {value:field.value,notes:document.querySelector('#book-notes')?.value,conflict:!!document.querySelector('.metadata-review[role=alert]')} : false;")


def persisted_chat_payloads(profile, job_ids):
    found = {}
    for path in profile.rglob("*"):
        if path.suffix not in {".sqlite", ".sqlite3", ".db"} or not path.is_file() or path.is_symlink():
            continue
        require(path.resolve(strict=True).is_relative_to(profile), "permissionDatabaseOutsidePrivateProfile")
        with path.open("rb") as stream:
            if stream.read(16) != b"SQLite format 3\x00":
                continue
        connection = sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True, timeout=5)
        try:
            connection.execute("PRAGMA query_only=ON")
            columns = {row[1] for row in connection.execute("PRAGMA table_info(jobs)")}
            if not {"id", "payload_json"}.issubset(columns):
                continue
            for identifier in job_ids:
                row = connection.execute("SELECT payload_json FROM jobs WHERE id=?", (identifier,)).fetchone()
                if row:
                    require(identifier not in found, "permissionJobFoundInMultipleDatabases")
                    envelope = json.loads(row[0])
                    require(isinstance(envelope, dict) and envelope.get("version") == 1
                            and isinstance(envelope.get("payload"), dict),
                            "permissionPersistedJobEnvelopeInvalid")
                    require(envelope.get("bookIds") == envelope["payload"].get("bookIds"),
                            "permissionPersistedJobEnvelopeScopeMismatch")
                    found[identifier] = envelope["payload"]
        finally:
            connection.close()
    require(set(found) == set(job_ids), "permissionJobsNotDurablyPersisted")
    return [found[identifier] for identifier in job_ids]


def reload_ui(driver, count, select=False):
    driver.execute("window.__fixtureReloadPending=true; setTimeout(()=>location.reload(),0); return true;")
    driver.wait(lambda: driver.execute("return window.__fixtureReloadPending!==true && document.querySelectorAll('.book-grid .book-card').length===arguments[0] && !!document.querySelector('#library-sort');", [count]), "fixtureUiReloadMissing")
    if select:
        click(driver, ".toolbar input[type=checkbox]", "fixtureSelectPageMissing")
        driver.wait(lambda: driver.execute("return document.querySelectorAll('.selection-control input:checked').length===arguments[0];", [count]), "fixtureSelectionMissing")


def consumed_permission_smoke(driver, profile, report):
    """Exercise real UI/IPC with a synthetic key in the isolated offline profile."""
    real_settings = driver.invoke("settings_get")
    require(real_settings["providerId"] is None and real_settings["modelId"] is None,
            "permissionBackendProviderUnexpectedlyConfigured")
    provider_id = "minimax"
    require(not any(provider["id"] == provider_id and provider["configured"]
                    for provider in driver.invoke("providers_list")), "permissionProviderAlreadyHasSecret")
    require(not any(job["kind"] == "enrich" for job in driver.invoke("jobs_list")),
            "permissionFixtureMustPrecedeBulkEnrichment")
    secret_installed = False
    cancelled = 0
    cancellation_attempts = 0
    observed_statuses = []
    try:
        driver.invoke("provider_set_secret", {"id": provider_id,
                      "secret": "synthetic-native-smoke-not-a-real-api-key", "persist": False})
        secret_installed = True
        temporary_settings = {**real_settings, "providerId": provider_id,
                              "modelId": "MiniMax-M2.7", "autoEnrich": False, "webEnabled": False}
        driver.invoke("settings_save", {"settings": temporary_settings})
        reload_ui(driver, 33, select=True)
        navigate(driver, True)
        driver.wait(lambda: driver.execute("return !document.querySelector('.configuration-hint');"), "permissionUiFixtureNotReady")
        checkbox = ".chat-composer input[type=checkbox]"
        click(driver, checkbox, "permissionCheckboxMissing")
        require(driver.execute("return document.querySelector(arguments[0])?.checked===true;", [checkbox]), "permissionFixtureDidNotEnable")

        def send_fixture(text):
            nonlocal cancelled, cancellation_attempts
            baseline = {job["id"] for job in driver.invoke("jobs_list") if job["kind"] == "chat"}
            require(driver.execute("const field=document.querySelector('#chat-input'); if(!field)return false; field.value=arguments[0]; field.dispatchEvent(new Event('input',{bubbles:true})); return true;", [text]), "permissionComposerMissing")
            driver.wait(lambda: driver.execute("return document.querySelector('.chat-composer button[type=submit]')?.disabled===false;"), "permissionFixtureCannotSend")
            click(driver, ".chat-composer button[type=submit]", "permissionSendButtonMissing")
            created = driver.wait(lambda: [job for job in driver.invoke("jobs_list")
                                           if job["kind"] == "chat" and job["id"] not in baseline],
                                  "permissionChatJobNotAccepted")
            require(len(created) == 1, "permissionSendCreatedMultipleJobs")
            job = created[0]
            observed_statuses.append(job["status"])
            require(job["status"] != "completed", "offlinePermissionJobUnexpectedlyCompleted")
            driver.wait(lambda: driver.execute("return document.querySelector('.chat-composer input[type=checkbox]')?.checked===false;"), "acceptedSendDidNotConsumePermission")
            def cancel_or_observe_terminal():
                # The UI consumes job_cancel's return value itself. Calling that
                # IPC externally would bypass trackJob and leave its view stale.
                clicked = driver.execute("const button=document.querySelector('.chat-job button');if(!button || button.disabled)return false;button.click();return true;")
                if clicked:
                    return "clicked"
                current = next((item for item in driver.invoke("jobs_list") if item["id"] == job["id"]), None)
                return "terminal" if current and current["status"] in {"completed", "failed", "cancelled"} else False

            cancellation = driver.wait(cancel_or_observe_terminal, "permissionUiCancellationNotReady", 10)
            if cancellation == "clicked":
                cancellation_attempts += 1
            settled = driver.wait(lambda: next((item for item in driver.invoke("jobs_list")
                                  if item["id"] == job["id"] and item["status"] in {"completed", "failed", "cancelled"}), False),
                                  "permissionChatJobDidNotSettle", 10)
            require(settled["status"] != "completed", "offlinePermissionJobUnexpectedlyCompleted")
            if cancellation == "clicked" and settled["status"] == "cancelled":
                cancelled += 1
            try:
                driver.wait(lambda: driver.execute("return document.querySelector('.chat-composer input[type=checkbox]')?.disabled===false;"), "permissionCancellationDidNotUnlockComposer", 10)
            except native.SmokeFailure:
                report["permissionUnlockDiagnostic"] = driver.execute("const checkbox=document.querySelector('.chat-composer input[type=checkbox]');const textarea=document.querySelector('#chat-input');return {checkboxExists:!!checkbox,checkboxDisabled:checkbox?.disabled,checkboxChecked:checkbox?.checked,textareaDisabled:textarea?.disabled,selectedChips:document.querySelectorAll('.selected-book-list .source-chip').length,pendingProgress:!!document.querySelector('.chat-job progress'),pendingCancelButton:!!document.querySelector('.chat-job button'),configurationHint:!!document.querySelector('.configuration-hint'),errorBanner:!!document.querySelector('.error-banner'),jobStatusLabel:document.querySelector('.chat-job .spread p.small')?.textContent?.slice(0,120)};")
                current = next((item for item in driver.invoke("jobs_list") if item["id"] == job["id"]), None)
                report["permissionUnlockDiagnostic"]["nativeJob"] = {"id": job["id"], "status": current["status"] if current else None,
                    "errorCode": current.get("error", {}).get("code") if current and isinstance(current.get("error"), dict) else None}
                raise
            return job["id"]

        first_id = send_fixture("Native permission fixture first request")
        second_id = send_fixture("Native permission fixture second request")
        payloads = persisted_chat_payloads(profile, [first_id, second_id])
        require(payloads[0].get("allowChanges") is True and payloads[1].get("allowChanges") is False
                and payloads[0].get("bookIds") == payloads[1].get("bookIds")
                and len(payloads[0].get("bookIds", [])) == 33
                and isinstance(payloads[0].get("conversationId"), str) and payloads[0]["conversationId"]
                and payloads[0].get("conversationId") == payloads[1].get("conversationId"),
                "permissionPersistedScopeOrConversationMismatch")
        report["permissionConsumption"] = {"configuration": "syntheticNonPersistentSecretInIsolatedOfflineProfile",
            "chatJobs": "realNativeIpc", "jobIds": [first_id, second_id], "requestAllowChanges": [True, False],
            "persistedPayloadReadOnlyVerified": True, "sameConversationAndSelection": True,
            "selectedBooks": 33, "observedJobStatuses": observed_statuses,
            "cancellationAttemptsViaNativeUi": cancellation_attempts,
            "cancelledViaOfficialCommand": cancelled}
    finally:
        try:
            driver.invoke("settings_save", {"settings": real_settings})
        finally:
            if secret_installed:
                try:
                    driver.invoke("provider_clear_secret", {"id": provider_id})
                except native.IpcFailure as error:
                    if error.code != "secretStoreUnavailable":
                        raise
                    report["permissionSecretCleanup"] = "systemVaultUnavailableSessionSecretRemoved"
                require(not any(provider["id"] == provider_id and provider["configured"]
                                for provider in driver.invoke("providers_list")),
                        "permissionSyntheticSecretNotCleared")
    require(driver.invoke("settings_get") == real_settings, "permissionFixtureChangedBackendSettings")
    require(not any(provider["id"] == provider_id and provider["configured"]
                    for provider in driver.invoke("providers_list")), "permissionSyntheticSecretNotCleared")
    reload_ui(driver, 33, select=True)
    navigate(driver, True)
    driver.wait(lambda: driver.execute("return !!document.querySelector('.configuration-hint');"), "permissionRealConfigurationNotRestored")


def smoke(driver, binary, profile, report):
    with checked(report, "freshNativeProfileWithoutProvider"):
        driver.start(binary)
        bootstrap = driver.invoke("app_bootstrap")
        settings = driver.invoke("settings_get")
        root = Path(settings["libraryRoot"]).resolve()
        require(root.is_relative_to(profile) and root != profile, "applicationDidNotUseIsolatedProfile")
        require(driver.invoke("library_list", {"query": native.QUERY})["total"] == 0, "libraryWasNotEmpty")
        settings.update({"language": "fr", "autoEnrich": False, "webEnabled": False,
                         "providerId": None, "modelId": None, "maxConcurrentJobs": 1})
        driver.invoke("settings_save", {"settings": settings})
        saved = driver.invoke("settings_get")
        require(not saved["autoEnrich"] and not saved["webEnabled"] and saved["providerId"] is None and saved["modelId"] is None, "offlineSettingsNotApplied")
        report["version"] = bootstrap["version"]
        report["providerConfigured"] = False

    with checked(report, "emptyAssistantOffersBookSelection"):
        navigate(driver, True)
        driver.wait(lambda: driver.execute("const panel=document.querySelector('.selected-books');const verify=panel?.querySelector(arguments[0]);const choose=panel?.querySelector(arguments[1]);return panel?.getBoundingClientRect().height>0 && verify?.disabled===true && choose?.disabled===false && panel.querySelectorAll('.source-chip').length===0;", [VERIFY_SELECTED, CHOOSE_BOOKS]), "emptyAssistantSelectionPanelMissing")
        click(driver, CHOOSE_BOOKS, "emptyAssistantChooseBooksMissing")
        driver.wait(lambda: driver.execute("return !!document.querySelector('#library-sort') && document.querySelectorAll('.book-card').length===0;"), "emptyAssistantChooseDidNotReturnLibrary")
        report["emptyAssistant"] = {"selectionPanelVisible": True, "verificationDisabled": True,
                                   "chooseBooksReturnsLibrary": True}

    with tempfile.TemporaryDirectory(prefix="library-manager-assistant-fixture-") as temporary:
        with checked(report, "officialImportOf33DistinctBooks"):
            paths = []
            for index in range(33):
                path = Path(temporary) / f"Assistant smoke {index:03d}.txt"
                path.write_text(f"Assistant smoke {index:03d}\n\nDistinct synthetic paragraph {index:03d}.\n", encoding="utf-8")
                paths.append(str(path))
            imported = driver.job(driver.invoke("import_books", {"paths": paths}))
            page = driver.invoke("library_list", {"query": native.QUERY})
            books = page["items"]
            require(page["total"] == 33 and len(books) == 33 and len({book["id"] for book in books}) == 33, "syntheticImportCountMismatch")
            require(imported["result"]["imported"] == 33, "syntheticImportResultMismatch")
            driver.wait(lambda: driver.execute("return document.querySelectorAll('.book-grid .book-card').length===33 && document.querySelector('main section.page')?.getAttribute('aria-busy')!=='true';"), "libraryCardsMissing")
            report["importedBooks"] = 33
            book = books[0]

        with checked(report, "mountedDraftAndFocusSurviveNativeRefreshStorm"):
            require(driver.execute("const node=[...document.querySelectorAll('.book-cover')].find(n=>n.getAttribute('aria-label')===arguments[0]); if(!node)return false; node.click(); return true;", [book["title"]]), "fixtureBookButtonMissing")
            driver.wait(lambda: title_field(driver), "bookPanelDidNotLoad")
            draft = "Assistant smoke unsaved draft"
            require(driver.execute("const node=document.querySelector('#book-title'); node.value=arguments[0]; node.dispatchEvent(new Event('input',{bubbles:true})); node.focus(); node.setSelectionRange(3,8); return document.activeElement===node;", [draft]), "draftFieldNotFocused")
            driver.wait(lambda: driver.execute("const button=document.querySelector('.drawer-footer button[type=submit]'); return button && !button.disabled;"), "draftWasNotDirty")
            metrics = driver.request("POST", driver.endpoint("/execute/async"), {"script": STORM_SCRIPT, "args": [book["id"]]})
            require(metrics.get("durationMs", 0) >= 3000 and metrics.get("emitted", 0) >= 40, "refreshStormWasNotMeasured")
            require(all(metrics.get(key, -1) == 0 for key in ("emitFailures", "disabled", "replaced", "draftLost", "focusLost")), "refreshStormDisturbedDraft")
            report["refreshStorm"] = metrics

        with checked(report, "remoteRevisionPreservesDraftUntilExplicitReload"):
            before = driver.invoke("book_get", {"id": book["id"]})
            remote_notes = "Assistant smoke remote notes"
            remote = driver.invoke("book_update", {"id": book["id"], "patch": {"notes": remote_notes}, "expectedRevision": before["revision"]})
            require(remote["revision"] > before["revision"], "remoteRevisionDidNotAdvance")
            state = driver.wait(lambda: (state if (state := title_field(driver)) and state["conflict"] else False), "dirtyDraftConflictNotVisible")
            require(state["value"] == draft, "remoteRevisionDiscardedDraft")
            require(driver.execute("return document.querySelector('.drawer-footer button[type=submit]')?.disabled===true;"), "conflictedDraftCanBeSaved")
            click(driver, ".metadata-review[role=alert] button", "conflictReloadMissing")
            driver.wait(lambda: (state if (state := title_field(driver)) and not state["conflict"] and state["value"] == remote["title"] and state["notes"] == remote_notes else False), "explicitReloadDidNotClearDraft")
            click(driver, ".drawer-header button", "bookPanelCloseMissing")

        with checked(report, "33SelectedBooksRetainAssistantAccessWithoutProvider"):
            script = "const done=arguments[arguments.length-1]; (async()=>{ const nodes=[...document.querySelectorAll('.selection-control input[type=checkbox]')]; for(const node of nodes){ if(!node.checked) node.click(); await new Promise(r=>setTimeout(r,0)); } done({count:nodes.length,checked:nodes.filter(n=>n.checked).length}); })().catch(()=>done({error:true}));"
            selection = driver.request("POST", driver.endpoint("/execute/async"), {"script": script, "args": []})
            require(selection.get("checked") == 33, "selectionDidNotInclude33Books")
            require(driver.execute("const assistant=document.querySelector('.selection-actions button.button.secondary');const verify=document.querySelector('.selection-actions button.button.primary');return !!assistant && !assistant.disabled && !!verify && verify.disabled;"), "librarySelectionActionsMissing")
            click(driver, ".toolbar button[aria-controls=library-facets]", "libraryFiltersButtonMissing")
            require(driver.execute("const sort=document.querySelector('#library-sort');const read=document.querySelector('#filter-read');if(!sort||!read)return false;sort.value='updated';sort.dispatchEvent(new Event('change',{bubbles:true}));read.value='unread';read.dispatchEvent(new Event('change',{bubbles:true}));return true;"), "libraryPreservationControlsMissing")
            driver.wait(lambda: (state if (state := library_view_state(driver)) and state["sort"] == "updated" and state["readStatus"] == "unread" and len(state["orderedTitles"]) == 33 else False), "libraryPreservationStateDidNotLoad")
            library_before = library_view_state(driver)
            click(driver, ".selection-actions button.button.secondary", "libraryAssistantActionMissing")
            driver.wait(lambda: driver.execute("return document.querySelectorAll('.selected-book-list .source-chip').length===33 && !!document.querySelector('.selected-books .selection-actions button.button.primary') && document.querySelector('.selected-books .selection-actions button.button.primary').disabled;"), "bulkAnalysisBlockedAt33Books")
            require(driver.execute("return document.querySelector('.chat-composer input[type=checkbox]')?.checked===false && !document.querySelector('.context-overflow');"), "assistantPermissionOrContextLimitIncorrect")
            click(driver, CHOOSE_BOOKS, "assistantChooseBooksMissing")
            driver.wait(lambda: library_view_state(driver) == library_before, "assistantReturnDiscardedLibraryState")
            report["libraryRoundTrip"] = {"selection": 33, "sort": "updated", "readFilter": "unread",
                                         "expandedFiltersPreserved": True, "paginationDisplayPreserved": True,
                                         "visibleBookOrderPreserved": True}
            click(driver, ".selection-actions button.button.secondary", "libraryAssistantReturnMissing")
            driver.wait(lambda: driver.execute("return document.querySelectorAll('.selected-book-list .source-chip').length===33;"), "assistantSelectionRoundTripMissing")

        def enrich_jobs():
            return [job for job in driver.invoke("jobs_list") if job["kind"] == "enrich"]

        with checked(report, "acceptedNativeSendConsumesPermissionForOneRequest"):
            consumed_permission_smoke(driver, profile, report)

        with checked(report, "bulkAnalysisDisabledWithoutConfiguredProvider"):
            require(not enrich_jobs(), "unexpectedPreexistingEnrichmentJobs")
            driver.wait(lambda: driver.execute("return document.querySelector(arguments[0])?.disabled===true;", [VERIFY_SELECTED]), "unconfiguredBulkAnalysisWasEnabled")
            click(driver, VERIFY_SELECTED, "disabledBulkAnalysisButtonMissing")
            require(not enrich_jobs(), "disabledBulkAnalysisEnqueuedJob")
            report["enrichmentJobs"] = {"count": 0, "disabledWithoutProvider": True,
                                        "configuredOfflineQueueCoveredByCatalogueMode": True}

        with checked(report, "selectedBookChipOpensFreshBookPanel"):
            require(driver.execute("const node=[...document.querySelectorAll('.selected-book-list .source-chip')].find(n=>n.textContent.includes(arguments[0])); if(!node)return false; node.click(); return true;", [book["title"]]), "selectedBookChipMissing")
            driver.wait(lambda: (state if (state := title_field(driver)) and state["value"] == book["title"] and state["notes"] == remote_notes else False), "selectedChipOpenedWrongOrStaleBook")
            click(driver, ".drawer-header button", "selectedBookPanelCloseMissing")
            report["proposalReview"] = {"status": "notExercised", "reason": "freshOfflineProfileHasNoCompletedProposal"}

        with checked(report, "modificationPermissionDefaultsAndResets"):
            checkbox = ".chat-composer input[type=checkbox]"
            require(driver.execute("return document.querySelector(arguments[0])?.checked===false;", [checkbox]), "permissionNotDisabledByDefault")
            click(driver, checkbox, "permissionCheckboxMissing")
            driver.wait(lambda: driver.execute("return document.querySelector(arguments[0])?.checked===true;", [checkbox]), "permissionCheckboxDidNotEnable")
            click(driver, ".chat-history > button", "newConversationButtonMissing")
            driver.wait(lambda: driver.execute("return document.querySelector(arguments[0])?.checked===false;", [checkbox]), "newConversationKeptPermission")
            click(driver, checkbox, "permissionCheckboxMissing")
            navigate(driver, False)
            driver.wait(lambda: driver.execute("return document.querySelectorAll('.book-grid .book-card').length===33;"), "libraryDidNotReturn")
            click(driver, ".selection-control input[type=checkbox]", "selectionCheckboxMissing")
            navigate(driver, True)
            driver.wait(lambda: driver.execute("return document.querySelectorAll('.selected-book-list .source-chip').length===32 && document.querySelector('.chat-composer input[type=checkbox]')?.checked===false;"), "changedSelectionKeptPermission")
            report["permissionReset"] = {"newConversation": True, "selectionNavigation": True}


def navigate_index(driver, index):
    require(driver.execute("const node=document.querySelectorAll('.nav-item')[arguments[0]]; if(!node)return false;node.click();return true;", [index]), "catalogueNavigationMissing")


def stored_bytes(root):
    hashes = {}
    for path in root.rglob("*"):
        require(not path.is_symlink(), "catalogueFixtureStorageSymlink")
        if path.is_file():
            require(path.resolve(strict=True).is_relative_to(root), "catalogueFixtureStorageEscape")
            hashes[str(path.relative_to(root))] = native.file_sha256(path)
    return hashes


def fixture_database(profile):
    databases = []
    for path in profile.rglob("*"):
        if not path.is_file() or path.is_symlink():
            continue
        with path.open("rb") as stream:
            if stream.read(16) != b"SQLite format 3\x00":
                continue
        require(path.resolve(strict=True).is_relative_to(profile), "catalogueFixtureDatabaseEscape")
        connection = sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True)
        try:
            names = {row[0] for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")}
            if {"books", "jobs"}.issubset(names):
                databases.append(path)
        finally:
            connection.close()
    require(len(databases) == 1, "catalogueFixtureDatabaseAmbiguous")
    return databases[0]


def seed_catalogue_proposals(profile, books, queued_jobs):
    """Only called on the fresh synthetic fixture after its application stops."""
    marker = profile / ".native-catalogue-actions-fixture.json"
    require(marker.is_file() and not marker.is_symlink(), "catalogueFixtureMarkerMissing")
    recorded = json.loads(marker.read_text(encoding="utf-8"))
    require(recorded.get("purpose") == "fresh-synthetic-catalogue-actions"
            and recorded.get("bookIds") == [book["id"] for book in books], "catalogueFixtureMarkerMismatch")
    connection = sqlite3.connect(fixture_database(profile), timeout=10)
    try:
        connection.execute("BEGIN IMMEDIATE")
        require(connection.execute("SELECT count(*) FROM books").fetchone()[0] == 2, "catalogueFixtureWasNotSynthetic")
        for index, book in enumerate(books):
            identifier = next(job["id"] for job in queued_jobs if job["bookIds"] == [book["id"]])
            title = "Catalogue fixture reviewed title" if index == 0 else book["title"]
            proposal = {"bookId": book["id"], "patch": {"title": title}, "confidence": 0.9,
                        "warnings": [], "providerId": "minimax", "modelId": "offline-fixture",
                        "evidence": [{"field": "title", "value": title, "confidence": 0.9, "sourceUrls": []}]}
            result = {"proposal": proposal, "review": {"state": "pending",
                      "sourceRevision": book["revision"], "reviewRevision": book["revision"]}}
            changed = connection.execute("UPDATE books SET metadata_status='needsReview' WHERE id=? AND revision=?",
                                         (book["id"], book["revision"])).rowcount
            require(changed == 1, "catalogueFixtureRevisionChanged")
            changed = connection.execute("UPDATE jobs SET status='completed',progress=1,result_json=?,error_json=NULL WHERE id=? AND kind='enrich' AND status='waitingForConfiguration'",
                                         (json.dumps(result), identifier)).rowcount
            require(changed == 1, "catalogueFixtureJobChanged")
        connection.commit()
    finally:
        connection.close()


def catalogue_removal_receipt(profile, expected_books):
    """Read only the marked synthetic fixture after its application stops."""
    marker = profile / ".native-catalogue-actions-fixture.json"
    require(marker.is_file() and not marker.is_symlink(), "catalogueReceiptMarkerMissing")
    require(marker.resolve(strict=True).is_relative_to(profile), "catalogueReceiptMarkerEscape")
    recorded = json.loads(marker.read_text(encoding="utf-8"))
    expected = sorted((book["bookId"], book["expectedRevision"]) for book in expected_books)
    require(recorded.get("purpose") == "fresh-synthetic-catalogue-actions"
            and len(expected) == 2 and len({identifier for identifier, _ in expected}) == 2
            and set(recorded.get("bookIds", [])) == {identifier for identifier, _ in expected},
            "catalogueReceiptFixtureMismatch")
    connection = sqlite3.connect(fixture_database(profile).resolve().as_uri() + "?mode=ro", uri=True)
    try:
        connection.execute("PRAGMA query_only=ON")
        require(connection.execute("SELECT count(*) FROM books").fetchone()[0] == 0,
                "catalogueReceiptBooksNotRemoved")
        rows = connection.execute("SELECT id,status,after_json FROM operations WHERE kind='catalogueRemove' ORDER BY id").fetchall()
        require(len(rows) == 2, "catalogueReceiptOperationCountMismatch")
        request_ids, removed_ids, operation_ids = set(), set(), set()
        for identifier, status, encoded in rows:
            receipt = json.loads(encoded)
            require(isinstance(receipt, dict) and status == "applied", "catalogueReceiptNotApplied")
            request_id = receipt.get("requestId")
            try:
                canonical_id = str(uuid.UUID(request_id))
            except (ValueError, TypeError, AttributeError):
                canonical_id = None
            require(canonical_id is not None and canonical_id == request_id,
                    "catalogueReceiptRequestIdInvalid")
            require(receipt.get("books") == [list(selection) for selection in expected],
                    "catalogueReceiptIntentMismatch")
            book = receipt.get("book")
            require(isinstance(book, dict)
                    and (book.get("id"), book.get("revision")) in expected,
                    "catalogueReceiptBookMismatch")
            request_ids.add(request_id)
            removed_ids.add(book["id"])
            operation_ids.add(identifier)
        require(len(request_ids) == 1 and len(operation_ids) == 2
                and removed_ids == {identifier for identifier, _ in expected},
                "catalogueReceiptBatchMismatch")
        return {"requestId": request_ids.pop(),
                "books": [{"bookId": identifier, "expectedRevision": revision}
                          for identifier, revision in expected]}, operation_ids
    finally:
        connection.close()


def undo_catalogue_operation(driver, operation_id, report):
    """Retry an idempotent undo once only if the same native session is alive."""
    try:
        result = driver.invoke("operation_undo", {"id": operation_id})
    except native.SmokeFailure as error:
        if str(error) != "webdriverUnreachable":
            raise
        # The tauri-driver proxy can lose an HTTP connection after a session
        # restart. Never recover by restarting a crashed application/session.
        require(driver.execute("return document.readyState==='complete' && typeof window.__TAURI_INTERNALS__?.invoke==='function';") is True,
                "catalogueUndoNativeSessionNotAlive")
        current = next((operation for operation in driver.invoke("operations_list")
                        if operation["id"] == operation_id), None)
        require(current is not None and current["kind"] == "catalogueRemove"
                and current["status"] in {"applied", "reverted"},
                "catalogueUndoUncertainOperationState")
        report.setdefault("catalogueUndoTransportRetries", []).append({
            "sameNativeSessionAlive": True,
            "observedOperationStatus": current["status"],
            "retryCount": 1,
        })
        result = driver.invoke("operation_undo", {"id": operation_id})
    require(result["status"] == "reverted", "catalogueUndoFailed")


def catalogue_actions_smoke(driver, binary, profile, report):
    """All writes and fixture SQL are confined to a new synthetic profile."""
    with checked(report, "freshSyntheticCatalogueActionsProfile"):
        driver.start(binary)
        bootstrap = driver.invoke("app_bootstrap")
        settings = driver.invoke("settings_get")
        root = Path(settings["libraryRoot"]).resolve(strict=True)
        require(root.is_relative_to(profile) and root != profile, "catalogueLibraryOutsidePrivateProfile")
        require(driver.invoke("library_list", {"query": native.QUERY})["total"] == 0, "catalogueProfileNotEmpty")
        settings.update({"language": "fr", "autoEnrich": False, "webEnabled": False,
                         "providerId": None, "modelId": None, "maxConcurrentJobs": 1})
        driver.invoke("settings_save", {"settings": settings})
        with tempfile.TemporaryDirectory(prefix="library-manager-catalogue-synthetic-") as temporary:
            paths = []
            for index in range(2):
                path = Path(temporary) / f"Catalogue fixture {index}.txt"
                path.write_text(f"Catalogue fixture {index}\n\nSynthetic distinct text {index}.\n", encoding="utf-8")
                paths.append(str(path))
            driver.job(driver.invoke("import_books", {"paths": paths}))
        books = driver.invoke("library_list", {"query": native.QUERY})["items"]
        require(len(books) == 2, "catalogueFixtureImportCountMismatch")
        marker = profile / ".native-catalogue-actions-fixture.json"
        with marker.open("x", encoding="utf-8") as target:
            os.chmod(marker, 0o600)
            json.dump({"purpose": "fresh-synthetic-catalogue-actions", "bookIds": [book["id"] for book in books]}, target)
        report["version"] = bootstrap["version"]
        report["profileMode"] = "freshSyntheticCatalogueActions"
        reload_ui(driver, 2, select=True)

    with checked(report, "metadataDisabledAcrossAllLibraryViewsWithoutProvider"):
        for index in range(8):
            navigate_index(driver, index)
            driver.wait(lambda: driver.execute("return !!document.querySelector('.selection-actions button.button.primary') && document.querySelector('.selection-actions button.button.primary').disabled;"), "unconfiguredMetadataActionEnabled")
        navigate_index(driver, 0)
        driver.wait(lambda: driver.execute("return document.querySelectorAll('.book-card').length===2;"), "catalogueLibraryDidNotReturn")
        click(driver, ".book-cover", "catalogueBookMissing")
        driver.wait(lambda: title_field(driver), "cataloguePanelMissing")
        require(driver.execute("return document.querySelector('.operations button.button.secondary')?.disabled===true && !!document.querySelector('#book-metadata-hint');"), "unconfiguredPanelEnrichmentEnabled")
        click(driver, ".drawer-header button", "cataloguePanelCloseMissing")
        require(not any(job["kind"] == "enrich" for job in driver.invoke("jobs_list")), "disabledMetadataEnqueuedJob")
        report["metadataReadiness"] = {"disabledViews": 8, "disabledBookPanel": True}

    with checked(report, "configuredSyntheticProviderAllowsOfflineMetadataQueue"):
        installed = False
        try:
            driver.invoke("provider_set_secret", {"id": "minimax", "secret": "synthetic-offline-catalogue-key", "persist": False})
            installed = True
            driver.invoke("settings_save", {"settings": {**settings, "providerId": "minimax", "modelId": "MiniMax-M2.7"}})
            reload_ui(driver, 2, select=True)
            driver.wait(lambda: driver.execute("return document.querySelector('.selection-actions button.button.primary')?.disabled===false;"), "configuredLibraryMetadataDisabled")
            for index in (1, 2, 3, 4, 5, 6, 7):
                navigate_index(driver, index)
                driver.wait(lambda: driver.execute("return document.querySelector('.selection-actions button.button.primary')?.disabled===false;"), "configuredMetadataViewDisabled")
            navigate_index(driver, 0)
            driver.wait(lambda: driver.execute("return document.querySelectorAll('.book-card').length===2;"), "configuredLibraryMissing")
            click(driver, ".book-cover", "configuredBookMissing")
            driver.wait(lambda: title_field(driver), "configuredBookPanelMissing")
            require(driver.execute("return document.querySelector('.operations button.button.secondary')?.disabled===false;"), "configuredPanelMetadataDisabled")
            click(driver, ".drawer-header button", "configuredPanelCloseMissing")
            before_jobs = {job["id"] for job in driver.invoke("jobs_list")}
            click(driver, ".selection-actions button.button.primary", "configuredBulkActionMissing")
            queued = driver.wait(lambda: (jobs if len(jobs := [job for job in driver.invoke("jobs_list") if job["kind"] == "enrich" and job["id"] not in before_jobs]) == 2 else False), "configuredBulkJobsMissing")
            require({identifier for job in queued for identifier in job["bookIds"]} == {book["id"] for book in books}, "configuredBulkScopeMismatch")
            for job in queued:
                current = next(item for item in driver.invoke("jobs_list") if item["id"] == job["id"])
                if current["status"] not in {"completed", "failed", "cancelled"}:
                    driver.invoke("job_cancel", {"id": job["id"]})
            driver.wait(lambda: all(job["status"] in {"completed", "failed", "cancelled"} for job in driver.invoke("jobs_list")), "offlineMetadataJobsDidNotSettle")
            require(not any(job["kind"] == "enrich" and job["status"] == "completed" for job in driver.invoke("jobs_list")), "offlineProviderUnexpectedlyCompleted")
            report["metadataReadiness"].update({"configuredViews": 8, "configuredBookPanel": True,
                                                "nativeQueuedBooks": 2, "fixtureProviderOnly": True})
        finally:
            driver.invoke("settings_save", {"settings": settings})
            if installed:
                try:
                    driver.invoke("provider_clear_secret", {"id": "minimax"})
                except native.IpcFailure as error:
                    if error.code != "secretStoreUnavailable":
                        raise
            require(not any(provider["configured"] for provider in driver.invoke("providers_list")), "catalogueSyntheticSecretNotCleared")

    with checked(report, "completedSyntheticProposalsSeededOnlyInStoppedFixture"):
        original_files = {book["id"]: [file for file in driver.invoke("book_files", {"id": book["id"]})
                          if file["variant"] == "original"] for book in books}
        original_digests = {file["sha256"] for files in original_files.values() for file in files}
        original_bytes = {path: digest for path, digest in stored_bytes(root).items() if digest in original_digests}
        require(len(original_bytes) == len(original_digests), "catalogueOriginalFilesMissing")
        fixture_jobs = [driver.invoke("book_enrich", {"id": book["id"]}) for book in books]
        driver.wait(lambda: all(next(job for job in driver.invoke("jobs_list") if job["id"] == queued["id"])["status"] == "waitingForConfiguration" for queued in fixture_jobs), "catalogueFixtureJobsNotWaiting")
        driver.close()
        seed_catalogue_proposals(profile, books, fixture_jobs)
        driver.start(binary)
        reload_ui(driver, 2)
        require(len([job for job in driver.invoke("jobs_list") if job["kind"] == "enrich" and job["status"] == "completed"]) == 2, "catalogueFixtureProposalsMissing")

    for index, original in enumerate(books):
        name = "differentProposalManualApplyDurable" if index == 0 else "noDifferenceProposalAcknowledgementDurable"
        with checked(report, name):
            require(driver.execute("const button=[...document.querySelectorAll('.book-cover')].find(n=>n.getAttribute('aria-label')===arguments[0]);if(!button)return false;button.click();return true;", [original["title"]]), "catalogueReviewBookMissing")
            driver.wait(lambda: driver.execute("return !!document.querySelector('section[aria-labelledby=metadata-proposal-title] button.button.primary:not(:disabled)');"), "catalogueReviewProposalMissing")
            before = driver.invoke("book_get", {"id": original["id"]})
            fields = driver.execute("return document.querySelectorAll('.metadata-review .proposal-field').length;")
            require((fields > 0) if index == 0 else (fields == 0), "catalogueReviewDifferencePresentationWrong")
            if index == 1:
                before = driver.invoke("book_update", {"id": original["id"], "patch": {"notes": "Synthetic personal note"}, "expectedRevision": before["revision"]})
                driver.wait(lambda: (state if (state := title_field(driver)) and state["notes"] == "Synthetic personal note" else False), "personalUpdateDidNotRefreshPanel")
                driver.wait(lambda: driver.execute("return !!document.querySelector('section[aria-labelledby=metadata-proposal-title] button.button.primary:not(:disabled)');"), "personalUpdateHidPendingProposal")
            click(driver, "section[aria-labelledby=metadata-proposal-title] button.button.primary", "catalogueReviewApplyMissing")
            applied = driver.wait(lambda: (current if (current := driver.invoke("book_get", {"id": original["id"]}))["revision"] > before["revision"] and current["metadataStatus"] == "verified" else False), "catalogueReviewNotPersisted")
            require(applied["title"] == ("Catalogue fixture reviewed title" if index == 0 else original["title"]), "catalogueReviewPatchMismatch")
            driver.wait(lambda: driver.execute("return !document.querySelector('section[aria-labelledby=metadata-proposal-title]');"), "catalogueResolvedReviewStillVisible")
            result = next(job["result"] for job in driver.invoke("jobs_list") if job["id"] == fixture_jobs[index]["id"])
            require(result["review"]["state"] == "applied" and result["review"]["resolvedRevision"] == applied["revision"], "catalogueReviewResolutionNotDurable")
            click(driver, ".drawer-header button", "catalogueReviewCloseMissing")
            reload_ui(driver, 2)
            require(driver.execute("return document.querySelectorAll('.review-action').length===arguments[0];", [1 - index]), "catalogueResolvedReviewReappeared")
            require(driver.execute("const button=[...document.querySelectorAll('.book-cover')].find(n=>n.getAttribute('aria-label')===arguments[0]);if(!button)return false;button.click();return true;", [applied["title"]]), "catalogueResolvedBookMissing")
            driver.wait(lambda: title_field(driver), "catalogueResolvedPanelMissing")
            require(driver.execute("return !document.querySelector('section[aria-labelledby=metadata-proposal-title]');"), "catalogueResolvedPanelReopenedReview")
            click(driver, ".drawer-header button", "catalogueResolvedPanelCloseMissing")

    with checked(report, "catalogueRemoveConfirmReceiptAndStoredBytesPreserved"):
        before_files = {book["id"]: driver.invoke("book_files", {"id": book["id"]}) for book in books}
        before_bytes = stored_bytes(root)
        require(all([file for file in before_files[identifier] if file["variant"] == "original"] == originals
                    for identifier, originals in original_files.items()), "catalogueReviewChangedOriginalRecords")
        require(all(before_bytes.get(path) == digest for path, digest in original_bytes.items()), "catalogueReviewChangedOriginalBytes")
        require(all(any(value == file["sha256"] for value in before_bytes.values()) for files in before_files.values() for file in files), "catalogueStoredFileHashMissing")
        expected_books = [{"bookId": book["id"],
                           "expectedRevision": driver.invoke("book_get", {"id": book["id"]})["revision"]}
                          for book in books]
        click(driver, ".toolbar input[type=checkbox]", "catalogueRemoveSelectMissing")
        click(driver, ".selection-actions .remove-action", "catalogueRemoveActionMissing")
        driver.wait(lambda: driver.execute("return document.querySelectorAll('dialog[open] .removal-books li').length===2 && document.querySelector('dialog[open] button[type=submit]')?.disabled===false;"), "catalogueRemoveConfirmationMissing")
        require(driver.invoke("library_list", {"query": native.QUERY})["total"] == 2, "catalogueRemovedBeforeConfirmation")
        click(driver, "dialog[open] button[type=submit]", "catalogueRemoveSubmitMissing")
        driver.wait(lambda: driver.invoke("library_list", {"query": native.QUERY})["total"] == 0, "catalogueRemoveNotPersisted")
        driver.wait(lambda: driver.execute("return !document.querySelector('dialog[open]') && document.querySelectorAll('.book-card').length===0;"), "catalogueRemovedBooksRemainInUi")
        driver.close()
        receipt, persisted_operation_ids = catalogue_removal_receipt(profile, expected_books)
        driver.start(binary)
        driver.wait(lambda: driver.execute("return !!document.querySelector('#library-sort') && document.querySelector('main section.page')?.getAttribute('aria-busy')==='false' && document.querySelectorAll('.book-card').length===0;"),
                    "catalogueRestartedEmptyLibraryNotReady")
        require(driver.invoke("library_list", {"query": native.QUERY})["total"] == 0,
                "catalogueRemovalDidNotSurviveRestart")
        repeated = driver.invoke("books_remove", receipt)
        require(set(repeated["removedBookIds"]) == {book["id"] for book in books} and len(repeated["operations"]) == 2, "catalogueRemovalReceiptMismatch")
        operations = [operation for operation in driver.invoke("operations_list") if operation["kind"] == "catalogueRemove"]
        require(len(operations) == 2 and {operation["id"] for operation in operations}
                == {operation["id"] for operation in repeated["operations"]}
                == persisted_operation_ids, "catalogueRepeatedRemovalCreatedOperations")
        require(stored_bytes(root) == before_bytes, "catalogueRemovalChangedStoredBytes")
        report["catalogueRemoval"] = {"removedBooks": 2, "explicitConfirmation": True,
                                      "receiptRetryIdempotent": True, "storedBytesUnchanged": True,
                                      "receiptSource": "stoppedMarkedFixtureReadOnlyAudit",
                                      "removalSurvivedRestart": True,
                                      "reviewOriginalBytesUnchanged": True}

    with checked(report, "catalogueUndoRestoresFreshUiAndActiveFiles"):
        for operation in repeated["operations"]:
            undo_catalogue_operation(driver, operation["id"], report)
        driver.wait(lambda: driver.invoke("library_list", {"query": native.QUERY})["total"] == 2, "catalogueUndoDidNotRestoreBooks")
        driver.wait(lambda: driver.execute("return document.querySelectorAll('.book-card').length===2 && document.querySelector('main section.page')?.getAttribute('aria-busy')==='false';"), "catalogueUndoUiDidNotRefresh")
        for book in books:
            require(sorted(driver.invoke("book_files", {"id": book["id"]}), key=lambda file: file["id"])
                    == sorted(before_files[book["id"]], key=lambda file: file["id"]), "catalogueUndoChangedActiveFileRecords")
        require(stored_bytes(root) == before_bytes, "catalogueUndoChangedStoredBytes")
        report["catalogueRemoval"].update({"undoRestoredBooks": 2, "undoUiRefreshed": True,
                                           "activeFilesRestored": True, "undoBytesUnchanged": True})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    profiles = parser.add_mutually_exclusive_group(required=True)
    profiles.add_argument("--profile-root", help="Fresh isolated XDG_DATA_HOME inherited by tauri-driver")
    profiles.add_argument("--review-profile", help="Marked private /tmp copy, never the host user profile")
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--driver-url", default="http://127.0.0.1:4444")
    parser.add_argument("--timeout", type=float, default=300)
    parser.add_argument("--catalogue-actions", action="store_true", help="Run 0.2.1 actions on a fresh synthetic profile only")
    arguments = parser.parse_args()
    report = {"schemaVersion": 1, "status": "running", "checks": [], "paidApiCalls": 0,
              "physicalDeviceWrites": 0, "networkIsolation": "requiredFromCaller"}
    driver = None
    exit_code = 1
    try:
        require(30 <= arguments.timeout <= 600, "smokeTimeoutOutOfRange")
        binary = arguments.binary.resolve(strict=True)
        require(binary.is_file() and os.access(binary, os.X_OK), "packagedApplicationNotExecutable")
        if arguments.review_profile:
            require(not arguments.catalogue_actions, "catalogueActionsRequireFreshSyntheticProfile")
            profile, marker = review_profile(arguments.review_profile)
        else:
            profile = native.isolated_profile(arguments.profile_root)
        report["binaryName"] = binary.name
        report["binarySha256"] = native.file_sha256(binary)
        driver = Driver(arguments.driver_url, arguments.timeout)
        if arguments.review_profile:
            review_smoke(driver, binary, profile, marker, report)
        elif arguments.catalogue_actions:
            catalogue_actions_smoke(driver, binary, profile, report)
        else:
            smoke(driver, binary, profile, report)
        report["status"] = "passed"
        exit_code = 0
    except (native.SmokeFailure, OSError, ValueError, KeyError, TypeError, sqlite3.Error) as error:
        report["status"] = "failed"
        report["errorCode"] = str(error) if isinstance(error, native.SmokeFailure) else "smokeInfrastructureOrContractError"
    finally:
        if driver:
            try:
                driver.close()
            except native.SmokeFailure:
                report["cleanupWarning"] = "webdriverSessionCloseFailed"
        try:
            arguments.report.parent.mkdir(parents=True, exist_ok=True)
            arguments.report.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        except OSError:
            report["status"] = "failed"
            report["errorCode"] = "smokeReportWriteFailed"
            exit_code = 1
        print(json.dumps(report, sort_keys=True))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
