# Inventaire de liseuse et import vers la bibliothèque

Demande du 9 octobre 2026 : sur une Xteink X4 Pro connectée en mode carte SD, l’application détecte le volume mais l’inventaire paraît bloqué. Afficher sa progression réelle, présenter les livres de la liseuse absents du catalogue local et permettre leur import individuel ou groupé.

Ce document suit cette correction. Les cases cochées indiquent uniquement des étapes vérifiées ; les résultats de la version précédente dans `MASTER_PLAN.md` ne valident pas ces nouveaux parcours.

## Contexte vérifié

Le projet utilise Tauri 2, un moteur Rust autonome, SQLite, Svelte 5 et des traductions FR/EN. Le moteur inventorie les montages accessibles ; les tâches transmettent leur état au shell par `job:updated`. Les livres de la liseuse et les livres locaux ont des identités distinctes : une présence sur carte ne crée pas automatiquement un livre local.

La cartographie complète existe dans `/home/rony/.Codex/projects/library-manager/memory/` : les douze fichiers attendus sont présents. Le signal d’absence vise un autre emplacement, sous `.claude`. Conserver les documents existants et les mettre à jour avec les changements de cette tâche. Le fichier `AGENTS.md` est absent à la racine du dépôt ; les instructions globales fournies dans la conversation s’appliquent.

## Suivi

- [x] Vérifier l’existence des douze documents de cartographie et préserver leur contenu.
- [x] Consigner la demande, le contexte technique et les critères de validation.
- [x] Identifier le travail coûteux de l’inventaire et le point où sa progression cesse d’être visible.
- [x] Définir les unités mesurables de progression, les étapes et la publication des résultats partiels.
- [x] Adapter le moteur, les contrats IPC et l’affichage de progression.
- [x] Afficher dans la bibliothèque les livres détectés sur l’appareil et absents du catalogue local, avec un indicateur explicite.
- [x] Ajouter l’import d’un livre de l’appareil et l’import groupé des livres absents du catalogue local.
- [x] Mettre à jour les libellés FR/EN et les documents de cartographie concernés.
- [x] Exécuter les tests ciblés et la validation de non-régression.
- [x] Vérifier le parcours rendu et distinguer les essais simulés des essais sur la liseuse physique.

## Critères de validation

| Parcours | Résultat attendu |
| --- | --- |
| Inventaire lent | L’interface affiche l’étape en cours et les unités déjà traitées. Le pourcentage correspond au travail mesuré ; une durée arbitraire ne fait pas avancer la barre. |
| Découverte en cours | Les résultats déjà disponibles deviennent visibles selon le mécanisme de publication retenu. Une interruption ne présente pas l’inventaire comme complet. |
| Livre déjà local | Le rapprochement conserve l’identité du livre local et sa présence sur l’appareil. |
| Livre absent du catalogue | La bibliothèque montre une entrée identifiable comme présente uniquement sur la liseuse, sans lui attribuer les actions qui exigent un fichier local. |
| Import individuel | Une action explicite copie le livre vers la bibliothèque gérée, conserve la source sur la carte et applique le dédoublonnage existant. |
| Import groupé | L’action cible les livres absents du catalogue. Elle restitue les réussites et les erreurs sans annoncer un succès global si un fichier échoue. |
| Déconnexion ou annulation | La tâche indique son état réel, retire la présence active après déconnexion et n’importe pas de fichier depuis une identité de volume devenue invalide. |
| Nouvel import du même contenu | Le catalogue réutilise le livre existant ; aucune entrée locale en double. |
| Français et anglais | Les libellés de présence, progression, import et erreur sont disponibles dans les deux langues. |

L’inventaire est une lecture de la carte. L’import utilise les protections du stockage local et revalide l’appareil avant lecture. Une liste tronquée destinée à l’interface ne doit pas devenir la source exhaustive de l’import groupé.

## Documentation à synchroniser

| Domaine modifié | Cartographie à actualiser |
| --- | --- |
| Inventaire, import et orchestration Rust | `backend_services.md` |
| Contrats de progression ou de livre détecté | `backend_models.md` |
| Commandes IPC | `backend_controllers.md`, `api_routes.md` |
| Schéma ou requêtes d’inventaire | `database_schema.md` |
| Bibliothèque, appareils et traductions | `frontend_spa.md` |
| Flux entre moteur et interface | `architecture_overview.md` |
| Cause de lenteur corrigée et limites restantes | `tech_debt.md` |

## Preuves de cette correction

Validation ciblée : 14 tests devices, 14 tests EPUB, 13 tests Manager (dont lot de 202 livres et refus du fichier modifié), un test import avec empreinte attendue et 9 tests preview passent. Les contrôles Svelte effectués ne signalent aucune erreur ni avertissement. Validation globale sur les sources figées `739d3dd` : 170 tests core réussis, un test réseau ignoré explicitement, huit tests Tauri et 24 tests frontend réussis ; Svelte/TypeScript sans erreur ni avertissement, format Rust, Clippy et build frontend réussis. La revue de non-régression conclut `findings: []` sur le périmètre inspecté. Le parcours matériel est consigné ci-dessous.

La release 0.1.1 demandée comprend DEB, RPM et AppImage, reconstruits avec Ubuntu 22.04 pour conserver glibc 2.35. La [CI Linux 37922847251](https://github.com/QrCommunication/library-manager/actions/runs/37922847251) et [CodeQL 37922845867](https://github.com/QrCommunication/library-manager/actions/runs/37922845867) ont réussi. Les essais natifs et matériels portent sur les paquets exacts de cette CI ; leurs résultats sont consignés ci-dessous. La version 0.1.1 est publiée ; les téléchargements publics et les empreintes du manifeste final sont vérifiés conformes.

## Livraison 0.1.1

- [x] Figer et pousser les sources de la correction (`739d3dd`).
- [x] Valider la CI Linux et l’exécution CodeQL.
- [x] Vérifier l’application native à partir du DEB exact de la CI.
- [x] Vérifier le contenu extrait de l’AppImage exact de la CI via AppRun.
- [x] Consigner l’inventaire sur la Xteink X4 Pro physique et distinguer lecture et copie.
- [x] Publier les paquets 0.1.1 et leurs rapports.
- [x] Télécharger les fichiers publics et vérifier leurs empreintes SHA-256.

Parcours natif du DEB exact de la CI : native-report.json, version 0.1.1, dix contrôles réussis et deux cartes de livres rendues ; SHA-256 du binaire b0fb95ede150f2d373ab0e991771226a03e5e7199332673f6957dd149bf45ad2. Capture facultative absente (screenshotSaved: false), reprise stricte sans capture réussie. Inspections des trois paquets réussies : application et compagnon GLIBC au plus 2.34, 172 ELF AppImage au plus 2.35, licences et libmobi 0.12 présents. Aucune preuve matérielle ou publique déduite de ces contrôles.

Contenu extrait de l’AppImage exact de la CI : appimage-extracted-report.json, version 0.1.1, dix contrôles et redémarrage réussis, deux cartes DOM ; empreintes binaire/compagnon conformes au manifeste. Réseau désactivé, aucune copie matérielle ni appel API payant ; capture absente et montage FUSE non testé. Ce résultat porte sur AppRun après extraction.

Parcours matériel : device-native-report.json, cinq contrôles réussis sur Xteink X4 Pro en mode carte SD, montage en lecture seule et profil local temporaire. Inventaire automatique de 134 livres/167 643 422 octets en 304,279 secondes, 543 échantillons de progression et résultats partiels observés. Bibliothèque locale initialement vide, panneau natif de 50 lignes appareil dans le DOM/sept visibles et importables. Import individuel TXT de 3 455 octets : local 0 → 1, inconnus 134 → 133, présence appareil conservée, réimport doublon 1, source inchangée deux fois. Capture privée sauvegardée hors publication ; zéro écriture carte et zéro appel API payant. L’import groupé physique n’est pas revendiqué.

Publication : [v0.1.1](https://github.com/QrCommunication/library-manager/releases/tag/v0.1.1), tag source 739d3dd. Contrôle anonyme terminé : douze fichiers publics, HTTP 200, onze empreintes du manifeste final conformes. Le rapport public décrit le premier état de onze fichiers/dix empreintes, puis son ajout a été contrôlé avec le manifeste final.

Vérification publique finale : [public-verification.json](https://github.com/QrCommunication/library-manager/releases/download/v0.1.1/public-verification.json), [SHA256SUMS](https://github.com/QrCommunication/library-manager/releases/download/v0.1.1/SHA256SUMS), publication le 2026-10-09 à 11:40:46 UTC. Résumé final local : final-public-verification.json, status passed, finalAssetCount 12, finalChecksumEntriesVerified 11, anonymousDownloads true, rapport et manifeste final HTTP 200/conformes aux originaux. Les parcours DEB, AppRun extrait et Xteink physique et leurs limites sont détaillés dans [QUALITY.md](QUALITY.md).
