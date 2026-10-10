# Changelog

Les notes détaillées sont en français, suivies d’un résumé en anglais.

## 0.2.1 — 2026-10-10

### Français

- Validation persistante des propositions de métadonnées : le livre, les éventuelles variantes, l’historique et l’état de revue sont enregistrés dans une transaction commune, avec contrôle de révision. Une proposition appliquée ne réapparaît plus après rafraîchissement ou redémarrage.
- Validation explicite possible même lorsque les valeurs proposées sont déjà identiques. Une édition personnelle conserve la proposition en attente et ajuste sa révision de revue ; elle ne la valide pas implicitement et ne permet pas d’écraser une modification concurrente.
- Barre d’actions groupées commune à toutes les vues de bibliothèque : Assistant, vérification des métadonnées, **Envoyer à la liseuse** et retrait du catalogue, jusqu’à **200 livres**. L’analyse exige un fournisseur sélectionné, configuré et prêt ainsi qu’un modèle renseigné ; les actions indisponibles expliquent leur condition.
- Transfert groupé avec choix d’un appareil connecté accessible en écriture, optimisation facultative et confirmation avant envoi. Le reçu de transfert reste associé à l’appareil choisi pendant le suivi de la tâche.
- Retrait confirmé du catalogue local avec contrôle de révision et reçu de demande idempotent : une répétition de la même demande ne crée pas de retrait ou d’historique en double. Les originaux, variantes et copies sur les appareils sont conservés ; l’historique permet une annulation avec vérification des fichiers et des collisions.
- Accès à une proposition conservé après une édition personnelle et son annulation, sans devoir recharger l’application. Les actualisations préservent les brouillons et le focus ; les livres sélectionnés dans l’assistant ouvrent une fiche fraîche.
- Inspection des vrais fichiers de l’édition avec vérification de leur intégrité et extraits bornés de pages de titre, de copyright et de chapitre. Les corrections de l’assistant restent limitées aux livres sélectionnés, avec permission consommée pour une seule demande ; une inspection partielle ne constitue pas une lecture intégrale.
- Installateurs natifs Windows x64 et macOS Intel/Apple Silicon, avec minimum macOS 13. Liaison statique du moteur MOBI Windows, publication relative aux handles et synchronisation réelle des répertoires. Une ouverture de profil refusée ne modifie plus les permissions du propriétaire actif. Le pipeline macOS sélectionne explicitement l’authentification API et contrôle séparément signature, notarisation et Gatekeeper.
- Publication depuis le commit exact du tag, conditionnée aux CI réussies, au contrôle des artefacts, licences, empreintes et parcours natifs Linux. Les téléchargements publics doivent être vérifiés avant promotion comme dernière version. Les notices tierces utilisent un inventaire canonique des dépendances de toutes les plateformes ; les rapports de la livraison constituent les preuves finales de ses paquets.

### English summary

Metadata proposal review is persisted atomically with the book, generated variants and operation history, with revision checks. Explicit review can acknowledge proposals whose values already match; personal edits retain pending proposals and update their review revision without silently accepting them or bypassing concurrent changes.

Shared bulk actions across library views support up to **200 books**, with provider/model readiness required for metadata analysis and explicit device selection and confirmation for transfers. Confirmed catalogue removal uses revision checks and idempotent request receipts, preserves original files, variants and device copies, and supports undo with file and collision checks.

Pending reviews remain accessible after personal edits and undo. Background refreshes preserve drafts and focus, and selected assistant books open fresh details. Inspection checks actual edition files and provides bounded title, copyright and chapter excerpts. Assistant write permission remains limited to one request and its selected books.

Native Windows x64 and macOS Intel/Apple Silicon packages target macOS 13 or later. Windows MOBI static linking, handle-relative publication and directory synchronization are corrected. A rejected second profile owner preserves the active owner’s permissions. Apple builds select the intended API authentication mode. The release workflow binds packages to the exact tagged source, checks licenses, hashes and Linux native scenarios, and requires verified anonymous downloads before promotion to latest. Third-party notices now use a canonical dependency inventory across platforms. Release manifests and validation reports record the final package evidence.

## 0.2.0 — 2026-10-10

### Français

- Correction de la bibliothèque bloquée à **0 livre** ou en chargement après un import, et des fiches qui clignotaient pendant les mises à jour de l’inventaire. Les actualisations regroupées conservent les résultats disponibles pendant les traitements en arrière-plan.
- Correction des analyses rejetées lorsque le fournisseur renvoyait un ISBN formaté ou une preuve encodée en JSON. Les valeurs et leurs preuves suivent la même normalisation, avec contrôle de l’ISBN, des types, des sources et de la révision. Les erreurs du fournisseur sont maintenant expliquées en français ou en anglais.
- L’assistant peut rechercher dans la bibliothèque et sur le Web, inspecter le vrai fichier d’une édition, puis utiliser ses métadonnées incorporées et ses pages de copyright pour étayer ses corrections. L’inspection fournit des **extraits bornés** et signale ses limites ; elle ne constitue pas une lecture intégrale du livre.
- Actions groupées visibles dans la Bibliothèque et l’Assistant : choisir les livres, ouvrir la sélection dans le chat et vérifier leurs métadonnées, jusqu’à **200 livres**. Les propositions à examiner sont accessibles même sans sélection et indépendamment des filtres courants.
- Revue explicite des propositions dans la fiche : comparaison des valeurs actuelles et proposées, consultation des preuves et application manuelle avec contrôle de révision. La disponibilité d’une proposition est affichée après la fin de l’analyse.
- Modification et organisation par l’assistant avec une autorisation limitée à **une demande et aux livres cochés**. Les fichiers réellement inspectés sont vérifiés avant correction ; les originaux sont conservés, les variantes sont distinctes et les opérations applicables peuvent être annulées.
- Cycle d’outils limité à huit étapes par réponse, avec une seule tentative supplémentaire pour corriger une réponse au format invalide. Les résultats exécutés sont journalisés ; une reprise réutilise les reçus de mutations terminées et refuse de réexécuter automatiquement une action interrompue au résultat incertain.
- Annulation et fermeture attendent la fin des opérations locales engagées. Une nouvelle analyse reste possible après un échec ; les demandes identiques déjà actives sont dédupliquées.

### English summary

Fixed the library remaining at zero books after imports and flickering book details during background inventory updates. Metadata validation now handles formatted ISBNs and JSON-encoded evidence consistently while preserving type, checksum, source and revision checks.

The assistant can inspect actual edition files, use embedded metadata and copyright excerpts, and perform authorized metadata updates or organization. Inspection is bounded and does not imply reading the entire book. Visible bulk actions support selections of up to 200 books, with explicit proposal review. Write permission applies to one request and its selected books; originals, separate variants and undo remain available. Tool cycles and format repair are bounded, durable receipts prevent repeated writes, and cancellation waits for local cleanup.

## 0.1.1 — 2026-10-09

### Français

- Inventaire USB avec progression calculée sur les octets réellement lus, compteurs de découverte et annulation pendant la lecture. Le dernier inventaire complet est conservé si la carte est déconnectée ou si le scan échoue.
- Lecture des seules métadonnées EPUB nécessaires à l’inventaire, sans décompresser les chapitres et images.
- Affichage progressif dans la bibliothèque des livres présents uniquement sur une liseuse USB, avec indication visuelle et pagination.
- Import individuel ou de tous les livres absents de la bibliothèque locale, sans modifier la carte SD. La sélection globale utilise l’inventaire complet du moteur, indépendamment des pages affichées.
- Vérification du montage, du chemin et de l’empreinte avant l’ajout local ; déduplication et actualisation de la présence sur la liseuse après import.

### English

USB inventory now reports measured read progress and exposes identified device-only books in the library. Import one book or all missing books while preserving SD-card originals. Inventory metadata reads are lighter, interrupted scans preserve the last complete snapshot, and native imports validate their source and deduplicate local copies.

## 0.1.0 — 2026-10-09

### Français

Première version de **Library Manager**, application Linux autonome sous licence **GPL-3.0-only**. Le moteur Rust, SQLite et le moteur MOBI sont embarqués ; l’utilisation de l’application ne nécessite pas d’installer Calibre, Node.js, Rust ou Python.

#### Bibliothèque et lecture

- Bibliothèque locale en grille de couvertures ou tableau, avec regroupement par auteur, série ou genre.
- Recherche dans les informations bibliographiques, ISBN, éditeur et notes locales ; filtres combinables par auteur, série, genre, étiquette, langue, format, état de lecture, favori, métadonnées à vérifier, couverture manquante, taille et présence sur un appareil.
- Tris par titre, auteur, série et numéro, ajout, modification, taille, progression, publication ou note ; numérotation prenant en charge le tome zéro et les positions décimales.
- Originaux conservés intacts, déduplication par SHA-256 et variantes classées selon **Auteur → Série → numéro et titre**. Notes, favoris, évaluations et progression restent associés au livre.
- Lecteur EPUB intégré avec sommaire, préférences de lecture et position sauvegardée. Contenu assaini dans une iframe isolée, avec scripts et ressources distantes bloqués.

#### Assistant et métadonnées

- Six fournisseurs API : **Z.ai, Kimi, MiniMax, Codex via OpenAI Responses, Claude et Mistral**. Catalogues de modèles récupérés à la demande depuis les API ou catalogues officiels, avec provenance, date et état du cache.
- Fenêtre de chat avec conversations locales, recherche dans la bibliothèque et sources publiques obtenues par le service Web commun à tous les fournisseurs. Le Web peut être désactivé dans les paramètres.
- Enrichissement automatique à l’import lorsque l’option est activée. Les traitements attendent une configuration valide sans bloquer l’accès aux livres ; les interruptions réseau suivent des reprises bornées.
- Propositions de métadonnées structurées, validation des champs et des preuves, seuil de confiance réglable et revue des corrections incertaines. Contrôle de révision avant application pour préserver les corrections humaines.
- Clés API conservées en session ou, sur demande, dans le coffre de secrets Linux. Notes personnelles, favoris, évaluations et progression exclus des corrections automatiques par le LLM.
- File de tâches persistante, suivi de progression, annulation coopérative, reprise après redémarrage et journal des opérations réversibles. Erreurs distinctes pour révision périmée, collision d’opération et profil déjà ouvert.

#### Formats et optimisation

- Conversions autonomes entre **EPUB, TXT, HTML et FB2**, avec sortie **MOBI6**. Reconstruction des fichiers **MOBI/KF8/AZW3 sans DRM** par le moteur libmobi embarqué.
- Les formats autres qu’EPUB sont conservés à l’import ; une conversion explicite vers EPUB permet leur lecture intégrée. **PDF et CBZ** restent catalogués dans leur format original, sans lecteur dédié ni conversion reflow. Aucun export AZW3/KF8 ni déchiffrement DRM.
- Quatre profils EPUB : **Sans perte**, **Équilibré**, **Xteink** et **Texte seul**. Adaptation des images, niveaux de gris, compression et retrait sûr des ressources selon le profil.
- Optimisation séparée ou avant transfert, avec variante dédiée et rapport de taille, images, polices, avertissements et contrôle du texte et des chapitres. Les transformations peuvent modifier la mise en page ; les originaux restent conservés.

#### Appareils et langues

- Détection et inventaire des volumes USB/SD montés, ainsi que des appareils MTP déjà accessibles via GVfs. Livres présents mis en évidence et filtrables uniquement pour les appareils actuellement connectés.
- Transferts USB et connexion HTTP directe à **CrossPoint**, avec vérification des copies et protection des fichiers existants.
- Serveur **Calibre sans fil intégré**, démarré explicitement pour un client compatible tel que KOReader, sans installation de Calibre. Connexion manuelle par adresse IP et port ; découverte UDP non disponible dans cette version.
- Interface **française et anglaise**, langue du système par défaut et sélecteur dans les paramètres. Dictionnaires JSON découverts automatiquement pour faciliter l’ajout de langues.

La compatibilité dépend du montage, des formats et des capacités du lecteur ou du firmware. Ces notes ne constituent pas une validation sur appareil physique, un résultat de test des paquets ou une preuve d’appel LLM payant. Les preuves de validation et le périmètre de distribution sont consignés séparément dans la documentation et les notes de publication.

### English summary

Initial release of **Library Manager**, a standalone Linux ebook library application licensed under **GPL-3.0-only**.

- Local cover grid and table, combined filters, author/series organization, immutable originals, content deduplication, personal notes and an isolated EPUB reader with saved progress.
- Six API providers—Z.ai, Kimi, MiniMax, OpenAI/Codex, Claude and Mistral—with dynamic model catalogs, shared Web research, chat and evidence-based metadata proposals. Durable jobs can wait for configuration; revision checks preserve manual edits.
- Built-in EPUB/TXT/HTML/FB2 conversions, MOBI6 output and bundled DRM-free MOBI/KF8/AZW3 reconstruction. PDF/CBZ are retained as originals. Four EPUB optimization profiles produce separate variants and reports.
- Mounted USB/SD discovery and presence filters, direct CrossPoint transfers and an integrated Calibre wireless server for compatible clients. Calibre installation is not required; wireless connections use a manually configured address.
- French and English interface, system language by default and automatically discovered JSON dictionaries for additional translations.

Device compatibility, native package validation and paid provider calls are separate from these feature notes; no universal or physically tested compatibility is claimed here.
