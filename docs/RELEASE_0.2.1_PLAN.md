# Livraison 0.2.1

Demande : validation persistante des propositions, actions communes sur toutes les vues de bibliothèque, installateurs Linux/macOS/Windows et notarisation macOS.

## État

- Version des manifestes : 0.2.1.
- Revue durable et raccord Library/Manager/IPC/UI : implémentés en source; tests ciblés réussis, contrôle Svelte sans erreur ni avertissement, 53 tests frontend réussis.
- SelectionActions, DeviceTransferDialog et RemoveBooksDialog : raccordés aux vues Bibliothèque/Appareils/Assistant; sélection bornée, confirmations et readiness fournisseur communes.
- Retrait catalogue : implémenté avec journal réversible, contrôle de révision, receipt idempotent et conservation des fichiers physiques; tests ciblés réussis.
- Sidecar natif : Linux réel validé avec sha2 0.11. Windows CI a révélé des primitives fichiers Unix dans le moteur; port sécurisé vers une façade Unix/Windows implémenté ; Database est encore en validation. MacOS et Windows nécessitent toujours une CI finale native.
- Signature Apple : matériel centralisé et sept secrets GitHub configurés via helper exécuté par l’utilisateur. Issuer retrouvé via Chrome Apple. Signature/notarisation/ticket restent à prouver dans la CI finale.
- Checkpoints plateforme 6b57b05 et correctif hash efd58c7 poussés. Validation globale finale, commit fonctionnel, CI exacte, release, téléchargement public et hashes : à faire.
- README FR/EN, BUILD, IPC, ENRICHMENT, METADATA_POLICY et CHANGELOG poussés dans 8b81301 ; CHAT révisé, BLUEPRINT/JOBS/guide/QUALITY en cours. Description GitHub actualisée.

## Contrats

Revue : enveloppe résultat `{proposal,review}` ; états pending/applied/dismissed/obsolete. La transaction CAS commune écrit livre, fichiers, revue et historique ; une édition personnelle ne consomme pas la proposition. Les propositions legacy restent lisibles et peuvent être traitées explicitement sans révision source fictive.

Sélection : au maximum 200 IDs distincts. Analyse disponible uniquement si fournisseur reconnu/configuré/prêt et modèle renseigné. Le transfert exige une cible connectée accessible en écriture et une validation explicite.

Retrait : `books_remove({requestId,books:[{bookId,expectedRevision}]})`. Retire du catalogue local, conserve originaux/variantes/copies appareil. Journal catalogueRemove réversible et receipt idempotent ; undo vérifie fichiers et collisions.

## Preuves de livraison

Séparer tests source, exécution native Linux, builds Windows/macOS, signature, notarisation Accepted/stapler/Gatekeeper et téléchargements publics. Ne pas attribuer à Windows 11 physique un test exécuté sur runner Windows Server. Ne pas publier de capture contenant la bibliothèque personnelle.

## Validation intermédiaire (sources non publiées)

Clippy workspace sur le snapshot fonctionnel Linux réussi. Ports actuels : secure_fs 7, storage 12, transfer 11, manager 26, library 24, conversion 12, inventaire EPUB 5 et devices 19 tests ciblés réussis ; Clippy core toutes cibles réussi. Ces contrôles ne remplacent pas la suite globale finale. Adaptateur Windows cross-compilé et Clippy GNU Windows réussi, sans exécution Windows encore.

Binaire local fonctionnel antérieur au portage : sept contrôles natifs réussis sur profil synthétique réseau isolé (huit vues, disponibilité IA, résolution durable après redémarrage, retrait confirmé, replay idempotent, conservation des octets). Le contrôle undo est interrompu par webdriverUnreachable et doit être relancé après correction du pilotage. Aucun appel API payant ni écriture appareil.

Natif R3 (profil neuf, network=none) : 8/8 contrôles réussis, dont retrait/replay après redémarrage et undo UI 0→2 avec fichiers/octets préservés. Rapport local `/tmp/library-manager-0-2-1-qa-r3/native-catalogue-report.json`, binaire SHA256 `004a5c9c23cc21040068d0abaf9c339f34096a0af77e380cd073ebac95c34056`. Snapshot fonctionnel antérieur aux ports FS ; à reprendre sur le paquet CI final. Aucun retry transport utilisé au passage réussi.

Suite globale Linux après portage : 295 tests core réussis, un test réseau volontairement ignoré ; workspace build réussi. Tests Tauri/Clippy workspace finaux et revues indépendantes en cours. Les sources fonctionnelles/portables sont figées pour le push et la CI native ; la release attend leurs résultats et ses archives exactes.
