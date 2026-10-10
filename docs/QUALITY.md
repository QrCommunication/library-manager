# Qualité et validations de Library Manager

## Version 0.2.1 — validations Linux et portage natif en cours

État au 10 octobre 2026 : les actions groupées communes, la revue persistante des propositions et le retrait réversible du catalogue sont en cours de livraison, avec le portage natif Windows et macOS. Les sources du commit `69641a0` ont réussi leur validation globale Docker, la CI Linux et les parcours natifs Linux général, assistant et catalogue. Des corrections supplémentaires du générateur de notices et de la liaison du moteur Windows sont validées localement ; leur CI sur le prochain commit reste attendue. Les installateurs Windows/macOS, la notarisation et la publication publique ne sont pas encore validés. Les résultats historiques des versions publiées restent conservés dans les sections suivantes.

### Sources actuelles : tests ciblés et analyse statique

| Périmètre ciblé | Résultat exécuté |
| --- | --- |
| Base de données | 12 tests réussis |
| Façade de fichiers sécurisés | Huit tests réussis |
| Stockage géré | 12 tests réussis |
| Transfert | 11 tests réussis |
| Tâches persistantes Jobs | 20 tests réussis, dont la publication atomique et les rollbacks |
| Repository de livres | 31 tests réussis |
| Manager | 29 tests réussis |
| Service de bibliothèque | 29 tests réussis |
| Enrichissement | 23 tests réussis |
| Paramètres | Quatre tests réussis |
| Conversion | 12 tests réussis, dont les conversions natives libmobi sur l’hôte Linux |
| Appareils | 19 tests réussis |
| Inventaire EPUB | Cinq tests réussis, avec lecture par fichier ouvert, limites et sources remplacées |
| Analyse statique du moteur | Clippy `library-core`, toutes cibles, avec `-D warnings`, réussi sur l’hôte Linux |
| Interface | 53 tests réussis ; contrôle TypeScript/Svelte sans erreur ni avertissement |
| Compilation croisée Windows | Neuf tests spécifiques compilés, **non exécutés** sur Windows |

Ces suites ciblées portent sur le portage, les gardes métier et la publication atomique des analyses ; leurs comptes ne sont pas à additionner pour annoncer une suite globale. Le résultat global après les derniers ports figure ci-dessous. La compilation croisée ne prouve ni l’exécution des tests Windows, ni le comportement de sa GUI ou de ses installateurs.

### Validation globale Docker du commit poussé

Le snapshot source exact [69641a07cf2602bb193e06c91b090f1be25e4846](https://github.com/QrCommunication/library-manager/commit/69641a07cf2602bb193e06c91b090f1be25e4846) a terminé sa validation globale Docker avec un code de sortie zéro. Le journal `final-workspace.log` conserve les résultats suivants :

| Contrôle global | Résultat |
| --- | --- |
| Compilation du workspace | Réussie |
| Moteur `library-core` | 315 tests réussis, aucun échec ; un test réseau volontairement ignoré |
| Coque Tauri | Huit tests réussis, aucun échec |
| Analyse statique Rust | Clippy workspace, toutes cibles, avec `-D warnings`, réussi |
| Format Rust | `cargo fmt --all -- --check` réussi |
| Notices tierces du snapshot `69641a0` | Sept autotests réussis ; ancien inventaire de 647 paquets et 43 avertissements vérifié sur Linux. Le nouvel inventaire commun aux plateformes est décrit ci-dessous |

Cette preuve globale porte sur le snapshot exact dans cet environnement Linux Docker. La CI Linux et les parcours du binaire issu de ce commit sont également réussis, comme décrit ci-dessous. Les changements ultérieurs des notices et de la liaison Windows exigent une nouvelle CI ; aucune signature ou notarisation macOS n’est déduite de ces résultats.

### Historique : parcours natif local du catalogue avant portage

Le scénario `scripts/native-assistant-smoke.py --catalogue-actions` a réussi **huit contrôles sur huit** dans un profil synthétique neuf et un conteneur Linux sans réseau externe. Le rapport local `native-catalogue-report.json` conserve l’empreinte du binaire exécuté : `004a5c9c23cc21040068d0abaf9c339f34096a0af77e380cd073ebac95c34056`.

Le scénario vérifie les actions de métadonnées indisponibles sans fournisseur dans les huit vues, leur activation avec une configuration synthétique et la création de deux analyses hors ligne. Il vérifie ensuite la validation durable d’une proposition avec changements et d’une proposition déjà identique, puis le retrait de deux livres, leur absence après redémarrage, la répétition idempotente du reçu et la restauration par les commandes officielles avec les fichiers conservés. Les propositions sont préparées uniquement dans la fixture marquée, application arrêtée. Le rapport indique zéro appel API payant et zéro écriture sur appareil physique.

Cette preuve porte sur un **binaire local figé avant les derniers ports**. Elle ne porte pas sur le futur binaire final de CI, ne valide pas Windows ou macOS et ne doit pas être présentée comme un rapport de livraison publique. Les parcours réussis du binaire CI `69641a0` sont documentés ci-dessous ; ils ne transforment pas ce rapport local antérieur en preuve du commit final de livraison.

### Baseline native CI R4 : défaut de cache reproduit

Le scénario catalogue a été étendu pour vérifier l’accès à une proposition après une édition personnelle et son annulation, sans rechargement ni remontage de l’interface. La baseline utilise le binaire du DEB CI [e351629](https://github.com/QrCommunication/library-manager/commit/e3516290fcc0d3c988e9ef56959a715f52551690), SHA-256 `3d0270511da701bc8c6dca96abac6dc5bc70d38a85292b23eb795a91a3266252`, dans un profil synthétique neuf et un conteneur sans réseau externe.

Les cinq premiers contrôles réussissent. Après l’édition officielle des notes et du favori, le livre est à la révision 2 et sa proposition reste durablement `pending`, avec `sourceRevision: 1` et `reviewRevision: 2`. Pourtant, après dix secondes, le DOM affiche deux cartes et zéro bouton **Examiner** sur les cartes, sans chargement en cours. Le bouton global reste visible. Le rapport R4 conserve l’échec `pendingReviewLibraryCacheStaleAfterPersonalEdit` ; le contrôle de validation sans différence et son contrôle imbriqué d’édition personnelle échouent sur cette même cause. Les étapes suivantes d’annulation personnelle et de retrait du catalogue ne sont pas atteintes.

Cette baseline établit un défaut de cache de l’interface malgré un réancrage correct de la proposition persistée. Les sources ont été corrigées pour actualiser les tâches et leurs résultats lorsque la bibliothèque change. Le parcours catalogue R5 du binaire CI `69641a0` réussit désormais le contrôle `pendingReviewRemainsReachableAfterPersonalEditAndUndo`, ainsi que les étapes suivantes de retrait et de restauration. La correction du cache est donc prouvée à l’exécution sur ce binaire Linux ; les prochains paquets restent soumis à leur propre validation. La baseline indique zéro appel API payant et zéro écriture sur appareil physique ; elle n’est pas un rapport de livraison réussi.

### Parcours natifs du binaire CI `69641a0`

Les rapports locaux suivants utilisent tous le binaire Linux produit par la CI du commit `69641a07cf2602bb193e06c91b090f1be25e4846`, SHA-256 `abc65b87bc671477f9b0418d0ed064b0f232074913d4c4e309c40304e9c341a1`. Les parcours général, assistant et catalogue s’exécutent dans des profils synthétiques distincts et des conteneurs sans réseau externe.

| Rapport local | Résultat et périmètre |
| --- | --- |
| `native-general-69641a0.log` | Dix contrôles réussis : import, déduplication, conversion TXT/EPUB et MOBI, lecteur/progression, optimisation Xteink, révisions et annulation, paramètres/langue et bibliothèque rendue |
| `native-assistant-69641a0-wait-report.json` | Dix contrôles réussis : sélection de 33 livres, retour bibliothèque conservant filtre/tri, ouverture d’une fiche fraîche, brouillon/focus préservés pendant 64 événements en 3 302 ms, permission consommée pour une seule demande et annulations officielles |
| `native-r5-69641a0.log` | Neuf contrôles réussis : configuration et file d’analyses hors ligne, validation durable avec ou sans différence, accès à la revue après édition personnelle et annulation, retrait idempotent puis restauration du catalogue et conservation des fichiers |
| `crosspoint-receipt-021-r4.json` | Quatre contrôles réussis avec le vrai transport HTTP sur un réseau Docker interne privé ; aucune liseuse physique utilisée |

Le premier essai assistant échouait dans la fixture : l’indication de configuration était visible avant que les titres des 33 boutons sélectionnés soient chargés. La reproduction sur le même binaire montre les boutons sans titre cible à 9 ms, puis le titre et la fiche fraîche à environ 328 ms. Le scénario attend désormais le bouton cible activé, sans délai fixe ni assouplissement du contrôle. La relance complète sur un profil neuf réussit les dix contrôles.

Ces rapports indiquent zéro appel API payant et zéro écriture sur appareil physique. Le scénario assistant neuf ne contient aucune proposition terminée : sa revue est explicitement non exercée, et la preuve correspondante vient du scénario catalogue. Les échanges CrossPoint restent une preuve du transport réel contre une fixture privée, sans preuve de firmware ou de matériel physique.

### Notices communes aux plateformes et liaison du moteur Windows

Après le checkpoint `69641a0`, le générateur recense l’union du graphe verrouillé pnpm avec `--lockfile-only`. L’installation verrouillée avec `--force --ignore-scripts` fournit les sources optionnelles de toutes les plateformes ; une source absente ou une identité de paquet divergente provoque un échec. Aucun paquet optionnel n’est exclu silencieusement. La génération canonique et son contrôle strict réussissent dans Docker avec **701 paquets et 67 avertissements d’inventaire des sources**, consignés dans [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Ces avertissements ne signifient pas que 67 dépendances sont distribuées sans licence.

Les **treize autotests** du générateur réussissent, notamment l’indépendance du graphe à la plateforme Linux/macOS, le refus d’une source optionnelle manquante et la résolution explicite du lanceur `pnpm.cmd` Windows. Cette dernière vérification simule le chemin de lancement Windows ; elle ne remplace pas son exécution native dans la CI Windows. Les versions et fichiers de verrouillage restent identiques.

Deux tests ciblés de `build.rs` réussissent sur l’hôte Linux avec libtool : la sélection du vrai fichier d’archive statique et le refus d’une DLL comme entrée de liaison statique. La correction vise l’échec Windows relatif à `zlib1.dll` ; son succès natif Windows reste à confirmer.

### CI et paquets restant à valider

La [CI Linux 38025145928](https://github.com/QrCommunication/library-manager/actions/runs/38025145928) du commit `69641a0` a réussi et produit les paquets utilisés par les parcours ci-dessus. La [CI native 38025145921](https://github.com/QrCommunication/library-manager/actions/runs/38025145921) a échoué : les notices différaient selon les sources optionnelles installées sur macOS, et la liaison Windows nécessitait une archive statique plutôt que `zlib1.dll`. Les corrections de ces deux causes sont présentes dans les sources locales et leurs tests ciblés réussissent ; les jobs natifs du nouveau commit restent attendus.

Sur le checkpoint macOS ARM de cette CI, 316 tests du moteur et six tests Tauri réussissent, ainsi que l’analyse statique. Ce résultat intermédiaire ne prouve pas la construction finale, la signature, la notarisation ou le parcours graphique macOS. Les installateurs Windows/macOS et les téléchargements publics de la 0.2.1 restent à valider ; aucune notarisation accomplie ni validation matérielle Windows n’est revendiquée.

Voir [le guide utilisateur](USER_GUIDE.md), [le plan de livraison 0.2.1](RELEASE_0.2.1_PLAN.md) et [les environnements et commandes de construction](BUILD.md). Le plan distingue la construction, la signature, la notarisation et la vérification des paquets effectivement publiés.

## Version 0.2.0 — assistant et revue des métadonnées

État au 10 octobre 2026 : CI, parcours natifs, publication et téléchargements publics validés. Les sources testées sont figées dans [fbcd2f7](https://github.com/QrCommunication/library-manager/commit/fbcd2f7c5828be6e83a9982a66bc85fa0bf7bbcc). Cette version rassemble les corrections du chargement de la bibliothèque et des fiches pendant l’inventaire, la normalisation commune des valeurs et preuves de métadonnées, les diagnostics du fournisseur et les outils d’inspection et de modification autorisée de l’assistant. Les actions de sélection et les accès à la revue sont maintenant visibles depuis la bibliothèque. Une proposition déjà appliquée ne reste pas présentée comme un changement à faire. Le détail du parcours et de ses garanties figure dans [ASSISTANT_METADATA_PLAN.md](ASSISTANT_METADATA_PLAN.md) et [CHANGELOG.md](../CHANGELOG.md).

L’accès **Examiner** repose sur une analyse terminée contenant une proposition valide et sur le livre concerné. Le statut `needsReview` seul ne prouve pas qu’une proposition existe. La sélection d’actions est limitée à 200 livres ; les modifications de l’assistant exigent une autorisation pour la demande envoyée et restent limitées aux livres sélectionnés. L’inspection lit les fichiers gérés, vérifie leur intégrité et transmet des extraits bornés, avec priorité aux pages de titre et de copyright. Ces extraits ne constituent pas une lecture intégrale du livre.

### Tests et construction sur le commit de livraison

| Périmètre | Résultat et provenance |
| --- | --- |
| Moteur Rust `library-core` | 240 tests réussis, un test réseau volontairement ignoré, aucun échec |
| Coque Tauri | Huit tests réussis |
| Interface frontend | 41 tests réussis |
| TypeScript/Svelte | Zéro erreur et zéro avertissement |
| Format et analyse Rust | `cargo fmt --all -- --check` et Clippy avec `-D warnings` réussis |
| Notices tierces | Sept autotests réussis |
| [CI Linux](https://github.com/QrCommunication/library-manager/actions/runs/38014777810) | Réussie sur `fbcd2f7c5828be6e83a9982a66bc85fa0bf7bbcc` ; construction des trois paquets Linux terminée |
| [CodeQL](https://github.com/QrCommunication/library-manager/actions/runs/38014777024) | Exécution réussie sur le commit de livraison ; ce statut ne ferme pas les alertes historiques documentées plus bas |

Ces résultats portent maintenant sur la CI du commit exact de livraison. Les preuves locales antérieures du paquet `.5`, documentées dans le plan assistant, restent un historique distinct.

### Parcours natifs du binaire produit par la CI

Trois rapports conservés dans `target/release-artifacts-v0.2.0/` indiquent `status: passed` et la même empreinte SHA-256 du binaire exécuté : `b331925c2cb1a41a54c4b73d2ba7c12dbefb3d42731a36887c0d90c27b173049`.

| Rapport public de la version v0.2.0 | Résultat natif contrôlé |
| --- | --- |
| [native-general-report.json](https://github.com/QrCommunication/library-manager/releases/download/v0.2.0/native-general-report.json) | Dix contrôles généraux réussis, version 0.2.0, deux cartes de livres rendues dans le DOM |
| [native-assistant-report.json](https://github.com/QrCommunication/library-manager/releases/download/v0.2.0/native-assistant-report.json) | Dix contrôles réussis : entrée à zéro sélection, choix de livres, conservation du filtre/tri et du brouillon pendant les actualisations, 33 analyses sans doublons et permission consommée pour une seule demande |
| [native-review-report.json](https://github.com/QrCommunication/library-manager/releases/download/v0.2.0/native-review-report.json) | Quatre contrôles réussis sur une copie privée : proposition persistée accessible, revue avant le formulaire, application manuelle puis disparition de la proposition appliquée, conservation des originaux et annulation officielle |

Les trois rapports indiquent zéro appel API payant et zéro écriture sur appareil physique. Une capture facultative du parcours général a rencontré un timeout WebKit. Le parcours a été relancé avec un profil neuf sans capture : les dix contrôles passent et le DOM contient deux cartes ; `screenshotSaved` vaut `false`. Aucun succès de capture n’est revendiqué.

### Publication et vérification publique

La [version v0.2.0](https://github.com/QrCommunication/library-manager/releases/tag/v0.2.0) a été publiée le 10 octobre 2026 à 02:04:06 UTC comme version stable et dernière version disponible au moment du contrôle. Elle distribue les paquets Linux [DEB](https://github.com/QrCommunication/library-manager/releases/download/v0.2.0/library-manager_0.2.0_amd64.deb), [RPM](https://github.com/QrCommunication/library-manager/releases/download/v0.2.0/library-manager-0.2.0-1.x86_64.rpm) et [AppImage](https://github.com/QrCommunication/library-manager/releases/download/v0.2.0/library-manager_0.2.0_amd64.AppImage), les sources du commit testé, les licences et les rapports de validation.

Les onze fichiers initiaux ont été téléchargés sans authentification avec HTTP 200 et correspondent aux artefacts publiés, comme le consigne [public-verification.json](https://github.com/QrCommunication/library-manager/releases/download/v0.2.0/public-verification.json). Les dix entrées du [SHA256SUMS](https://github.com/QrCommunication/library-manager/releases/download/v0.2.0/SHA256SUMS) téléchargé sont conformes. Le rapport de vérification a ensuite été ajouté comme douzième fichier et téléchargé à son tour avec HTTP 200 ; son SHA-256 contrôlé séparément est `4a531d87d41c9cbc578d8fe05a44be17f445fb9ac4db4e075681c4e4f2a1ea23`. L’empreinte du DEB public est `fa241b3b127bcbdaffbeb6ebf18184aebc9d0fecd04a04798d32ef3c19525586`. Les trois rapports natifs liés ci-dessus font partie des fichiers publics téléchargés et vérifiés.

La fenêtre réelle du binaire CI 0.2.0 a également été contrôlée sur le profil de l’utilisateur : 134 livres et le bouton **Examiner les propositions** sont visibles. Le processus observé est `720055`. Sa capture reste privée parce qu’elle contient la bibliothèque de l’utilisateur. Ce contrôle lance le binaire extrait du paquet CI ; le système conserve encore le paquet local `.5`, et aucune installation administrateur de 0.2.0 n’est revendiquée.

La construction DEB/RPM/AppImage et ces parcours ne prouvent ni le montage FUSE de l’AppImage, ni un nouveau parcours GUI du RPM, ni une nouvelle compatibilité matérielle. Les preuves historiques `.5`, 0.1.1 et 0.1.0 restent distinctes.

## Version 0.1.1 — inventaire USB et import depuis la liseuse

État au 9 octobre 2026. Les sources de cette correction sont figées dans [739d3dd](https://github.com/QrCommunication/library-manager/commit/739d3ddf874b794dbe762027dd3df7c4ee1d3269). L’inventaire USB publie les étapes et les octets effectivement lus ; les livres présents uniquement sur la liseuse apparaissent dans la bibliothèque et peuvent être importés individuellement ou par lot. Les détails du suivi figurent dans [DEVICE_INVENTORY_PLAN.md](DEVICE_INVENTORY_PLAN.md).

### Tests et construction

| Périmètre | Résultat de la correction 0.1.1 |
| --- | --- |
| Moteur Rust `library-core` | 170 tests réussis ; un test réseau ignoré explicitement ; aucun échec |
| Coque Tauri | Huit tests réussis |
| Interface TypeScript/Svelte | 24 tests réussis ; contrôle sans erreur ni avertissement |
| Format et analyse Rust | `cargo fmt --all -- --check` et Clippy workspace/toutes cibles avec `-D warnings` réussis |
| Construction frontend | Build Vite réussi |
| Revue de non-régression | `findings: []` sur les modifications inspectées ; cette revue ne remplace pas les essais matériels |
| [CI Linux](https://github.com/QrCommunication/library-manager/actions/runs/37922847251) | Réussie sur les sources de la correction ; construction des paquets Linux terminée |
| [CodeQL](https://github.com/QrCommunication/library-manager/actions/runs/37922845867) | Exécution réussie ; ce statut ne ferme pas les alertes historiques documentées plus bas |

Les nouveaux tests utilisent des fichiers synthétiques et des montages simulés. Ils couvrent la progression mesurée et monotone, les résultats partiels, l’annulation, les chemins et fichiers modifiés, le contrôle du SHA-256 attendu avant insertion locale, le rapprochement de présence, les doublons et l’import complet au-delà de 200 livres. Le scénario de lot sélectionne 202 livres : 201 sont importés et un fichier modifié après inventaire est refusé avec une erreur distincte. Les tests de démonstration conservent la séparation entre livres locaux et fichiers présents uniquement sur liseuse et refusent toute copie réelle.

### Paquets et parcours natif du DEB

Le DEB 0.1.1 exact produit par la CI a été exécuté dans l’application native. Son [rapport native-report.json](https://github.com/QrCommunication/library-manager/releases/download/v0.1.1/native-report.json) indique `status: passed`, dix contrôles réussis, deux cartes de livres rendues dans le DOM et zéro appel API payant ou écriture sur appareil physique. Les contrôles portent sur démarrage, paramètres et six adaptateurs, import/dédoublonnage, conversion TXT vers EPUB et lecteur/progression, optimisation Xteink, compagnon MOBI, révisions/annulation d’opération, attente de configuration IA, langue après redémarrage et bibliothèque rendue. L’empreinte SHA-256 du binaire exécuté est `b0fb95ede150f2d373ab0e991771226a03e5e7199332673f6957dd149bf45ad2`.

`screenshotSaved` vaut `false`. Une tentative de capture facultative a échoué ; une nouvelle exécution stricte sans capture a réussi les dix contrôles. La présence des deux cartes est établie par le DOM natif ; aucune capture de ce parcours standard n’est jointe au rapport.

Les trois formats DEB, RPM et AppImage ont passé l’inspection des paquets. Le binaire de l’application et son compagnon exigent au plus GLIBC 2.34 ; les 172 ELF inspectés dans l’AppImage exigent au plus GLIBC 2.35. Les licences et le compagnon libmobi 0.12 sont présents. L’inspection du RPM ne constitue pas un parcours GUI de ce paquet dans cette correction.

### Parcours du contenu extrait de l’AppImage

Le contenu exact de l’AppImage produit par la CI a été exécuté via son `AppRun` extrait. Le [rapport appimage-extracted-report.json](https://github.com/QrCommunication/library-manager/releases/download/v0.1.1/appimage-extracted-report.json) indique `status: passed`, version 0.1.1, dix contrôles réussis et deux cartes de livres rendues dans le DOM. Le redémarrage et la conservation de la langue passent également. L’essai fonctionne avec réseau désactivé, sans appel API payant ni écriture sur appareil physique.

Le rapport confirme la correspondance des empreintes du binaire et du compagnon au manifeste de construction. L’empreinte du binaire embarqué est `8bd501b95376146d3f74856d301c1bb057ceab602c406fd9d434ab6b0fed6ca0` ; celle du paquet AppImage est `b320ca0d0feeafd9b3633169f919036bfd2756a92089a1acb03c91533498b00a`. La capture reste absente (`screenshotSaved: false`). `fuseMountTested` vaut `false` : ce résultat valide le contenu lancé par extraction, sans valider le montage FUSE ni le cycle de l’enveloppe AppImage.

### Inventaire et import sur la Xteink X4 Pro physique

Le DEB exact de la CI a été exécuté avec la Xteink X4 Pro connectée en mode carte SD et un profil local temporaire isolé. Le [rapport device-native-report.json](https://github.com/QrCommunication/library-manager/releases/download/v0.1.1/device-native-report.json) indique `status: passed` pour les cinq contrôles matériels, avec provenance CI 37922847251/artifact 11612354741, version 0.1.1 et même empreinte de binaire que le parcours DEB ci-dessus.

L’inventaire automatique a terminé la lecture de 134 livres, soit 167 643 422 octets, en 304,279 secondes (environ cinq minutes et quatre secondes). Le rapport conserve 543 échantillons de progression ; les octets lus augmentent réellement jusqu’au total attendu. Les livres déjà analysés deviennent accessibles avant la complétion. La bibliothèque locale commence vide : le panneau contient 50 lignes de livres présents uniquement sur liseuse dans le DOM, dont sept visibles et importables dans la fenêtre.

L’essai importe vers le profil local un livre TXT de 3 455 octets. Le catalogue passe de zéro à un livre, sa présence sur la liseuse est reconnue et le nombre de livres absents du catalogue passe de 134 à 133. Un second import du même fichier produit un doublon, avec source inchangée après les deux opérations. Le montage physique est en lecture seule (`readOnlyPhysicalMount: true`) : aucune écriture sur la carte n’a été effectuée. L’import groupé au-delà de 200 livres est vérifié par tests synthétiques ; cet essai matériel porte sur l’import individuel et sa répétition.

Une capture du parcours matériel a été enregistrée en privé. Elle reste hors des fichiers publics parce qu’elle contient la bibliothèque de l’utilisateur. Le réseau externe est désactivé et aucun appel API payant n’a été effectué. Ces résultats valident cette liseuse et cette connexion, sans généraliser à tous les volumes, firmwares ou transports.

### Publication et vérification publique

La [version v0.1.1](https://github.com/QrCommunication/library-manager/releases/tag/v0.1.1) a été publiée le 9 octobre 2026 à 11:40:46 UTC. Son tag pointe vers les sources `739d3ddf874b794dbe762027dd3df7c4ee1d3269`, qui ont produit les paquets testés.

Le contrôle initial a téléchargé les onze fichiers alors publiés sans authentification, avec HTTP 200 et correspondance aux originaux locaux. Les dix empreintes du manifeste initial sont conformes. Le [rapport public-verification.json](https://github.com/QrCommunication/library-manager/releases/download/v0.1.1/public-verification.json) consigne ce premier état de onze fichiers ; son ajout porte la version à douze fichiers. Le [SHA256SUMS final](https://github.com/QrCommunication/library-manager/releases/download/v0.1.1/SHA256SUMS) contient désormais onze empreintes, dont celle du rapport public, toutes vérifiées conformes. Le rapport publié et le manifeste final ont également été téléchargés sans authentification avec HTTP 200 et correspondent aux fichiers locaux.

Le [manifeste de construction](https://github.com/QrCommunication/library-manager/releases/download/v0.1.1/BUILD_MANIFEST.json) relie source, CI, paquets et binaires. Les douze fichiers publics comprennent trois paquets Linux, l’archive de sources, les licences/notices, le manifeste, les trois rapports natifs, le rapport de vérification publique et les sommes SHA-256. La capture matérielle privée reste hors publication. Les preuves de v0.1.0 ci-dessous restent conservées comme historique distinct.

## Historique — version 0.1.0

Compte rendu du 9 octobre 2026. Les paquets Linux, le moteur et l’interface ont été vérifiés dans les périmètres décrits ci-dessous. Les alertes de sécurité ouvertes, les limites du test AppImage et les fonctions sans essai matériel restent explicites.

### Sources et preuves

Les paquets finaux ont été construits depuis [3e6177b](https://github.com/QrCommunication/library-manager/commit/3e6177b1241e35e3ef1cdf2ac6b6466d5742f775). Le code Rust et JavaScript exécuté reste celui de [8f4de47](https://github.com/QrCommunication/library-manager/commit/8f4de47ab7800f69e3faf02c9d1fc10ea34b6b68) : les changements suivants portent sur les fixtures de test, la CI, le script de validation native, le générateur et le contenu des notices, ainsi que la déclaration de `ca-certificates` comme dépendance des paquets DEB/RPM. La [CI Linux de 3e6177b](https://github.com/QrCommunication/library-manager/actions/runs/37887629001) a réussi en 18 min 36 s : tests, Clippy, notices, construction des trois bundles, collecte et dépôt des artefacts. Les trois paquets et leurs parcours natifs décrits ici ont terminé leur validation. Le tag public v0.1.0 pointe vers [47f9149](https://github.com/QrCommunication/library-manager/commit/47f9149), qui ajoute uniquement la documentation de livraison aux sources des paquets.

La [version publique v0.1.0](https://github.com/QrCommunication/library-manager/releases/tag/v0.1.0) a été publiée le 9 octobre 2026 à 05:32:29 UTC. Ses treize fichiers initiaux ont été téléchargés sans authentification par `curl`, avec réponse HTTP 200 pour chacun : trois paquets, une archive de sources, quatre rapports, quatre captures et `SHA256SUMS`. Les douze empreintes du manifeste initial correspondent aux fichiers téléchargés. Ce contrôle est consigné dans [public-verification.json](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/public-verification.json), qui conserve le compte initial de treize fichiers.

Ce rapport de vérification a ensuite été publié comme quatorzième fichier de la version. Il a été téléchargé sans authentification avec réponse HTTP 200, ainsi que le `SHA256SUMS` final de 1 215 octets. Ce dernier contient treize empreintes, toutes conformes aux fichiers téléchargés, et est identique octet par octet à l’original local. Le contrôle du manifeste final a utilisé une requête sans cache pour éviter la réponse mise en cache de sa première version. Les rapports et captures publics sont donc les mêmes que les preuves contrôlées localement :

| Preuve publique | Résultat contrôlé |
| --- | --- |
| [Rapport DEB](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/native-report.json) et [capture](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/native-window.png) | Paquet DEB : dix étapes réussies, deux cartes visibles et capture enregistrée |
| [Rapport Ubuntu vierge](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/ubuntu-clean-report.json) et [capture](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/ubuntu-clean-window.png) | DEB final installé dans Ubuntu 22.04 vierge : certificats installés automatiquement, dix étapes réussies et deux cartes visibles |
| [Rapport RPM Fedora](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/fedora-rpm-report.json) et [capture](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/fedora-rpm-window.png) | RPM final installé dans Fedora 44 : dix étapes réussies et deux cartes visibles |
| [Rapport AppRun extrait](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/appimage-extracted-report.json) et [capture](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/appimage-extracted-window.png) | Contenu extrait de l’AppImage lancé par `AppRun` : dix étapes réussies et deux cartes visibles |
| [SHA256SUMS](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/SHA256SUMS) | Treize empreintes finales vérifiées pour les paquets, sources, rapports, captures et preuve de vérification publique |
| [Archive des sources](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/library-manager-0.1.0-source.tar.gz) | 8 172 465 octets ; sources de la livraison avec le moteur et les licences tierces |

Les scripts de contrôle sont publics : [validation native](../scripts/native-smoke.py), [génération des notices](../scripts/third-party-notices.py) et [CI](../.github/workflows/ci.yml). [BUILD.md](BUILD.md) donne les commandes et l’environnement nécessaires à leur reproduction. Les parcours locaux et les téléchargements publics ont été vérifiés séparément.

### Vérifications automatisées

| Périmètre | Résultat |
| --- | --- |
| Moteur Rust `library-core` | 158 tests réussis, un test réseau explicitement ignoré, aucun échec |
| Coque Tauri | Huit tests réussis : six helpers du pont et deux contrôles d’instance unique |
| Interface TypeScript/Svelte | 22 tests réussis ; contrôle de types et de composants sans erreur ni avertissement |
| Format Rust | `cargo fmt --all -- --check` réussi |
| Analyse Rust | Clippy sur tout le workspace et toutes les cibles, avec `-D warnings`, réussi |
| Notices tierces | Sept autotests réussis ; génération et contrôle reproductibles de 647 packages du graphe actif pnpm |
| Script natif | Neuf autotests réussis ; compilation Python et contrôle des différences réussis |

Les tests couvrent notamment les chemins et liens symboliques, les collisions, la conservation des originaux, l’import et la déduplication, les révisions concurrentes, les tâches persistantes, l’annulation, les filtres, le lecteur isolé, les formats et les contrats des fournisseurs. Les 43 limites d’inventaire de sources sont décrites dans [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Ce nombre concerne l’inventaire de dépendances toutes plateformes ; il ne signifie pas que 43 bibliothèques sont livrées sans licence dans les paquets Linux.

### Parcours natif du DEB

Le vrai binaire du DEB final a été exécuté dans un environnement Ubuntu 22.04 isolé avec GTK/WebKit, Xvfb et WebDriver, puis dans un Ubuntu 22.04 vierge après installation par `apt`. Le script utilise le pont IPC de l’application. Il ne charge pas la démonstration et n’ajoute aucun plugin de test à la production.

Le rapport final indique `status: passed`, `renderedBookCount: 2` et `screenshotSaved: true`. Ses dix étapes vérifient :

1. La fenêtre native, le démarrage et le profil temporaire dédié.
2. Les six adaptateurs API et l’enregistrement des paramètres.
3. L’import réel et la déduplication par empreinte.
4. La conversion explicite TXT vers EPUB, le texte du lecteur et la progression sauvegardée.
5. L’optimisation Xteink, la conservation du texte et de la source.
6. La sortie MOBI native, l’import indépendant du MOBI synthétique et sa conversion explicite vers EPUB avec libmobi embarqué.
7. La modification des métadonnées, le refus d’une révision périmée et l’annulation de l’opération.
8. La mise en attente de l’enrichissement IA quand aucun fournisseur n’est configuré.
9. La conservation de la langue et de la bibliothèque après redémarrage.
10. Deux cartes de livres réellement visibles, un état stable puis plusieurs images de rendu avant la capture.

Le format source effectif de la sortie MOBI est TXT dans ce parcours. Le test ne présente donc pas cette étape comme une conversion EPUB vers MOBI. Les livres et les contenus utilisés dans le rapport natif sont synthétiques. L’empreinte du binaire DEB exécuté est `37305efbbf10183252279f6c00cf4ae417f4165aab58e1f55bae8d65cc278dc8`.

### Installation et AppImage

Le DEB final a été installé dans un Ubuntu 22.04 vierge et le RPM final dans Fedora 44. Les contrôles de version, de présence du moteur compagnon et de résolution des bibliothèques système passent. Dans Ubuntu, `ca-certificates` était absent avant l’installation : `apt` l’a installé automatiquement avec les dépendances GTK/WebKit déclarées par le DEB. Les métadonnées effectives des deux paquets contiennent les dépendances certificats, GTK et WebKit. Ces environnements n’installent ni Calibre, ni Node.js, ni Rust pour utiliser Library Manager.

Les parcours GUI complets des deux paquets finaux réussissent dans ces environnements : dix étapes, deux cartes visibles et capture enregistrée dans `ubuntu-clean-report.json` et `fedora-rpm-report.json`. Le parcours Fedora vérifie également le redémarrage et la conservation de la bibliothèque et de la langue. Ces résultats concernent Ubuntu 22.04 et Fedora 44 ; ils ne prouvent pas une compatibilité avec toutes les distributions.

L’AppImage finale mesure 85,49 MiB. Sur un premier artefact, dont l’empreinte était `21cbdbbf1a732553165f06245dddd000294e416ad371fcb5d3d8b725526d074c`, le lancement par `APPIMAGE_EXTRACT_AND_RUN=1` et les huit premières étapes du parcours natif passaient. Le redémarrage dans cette même session WebDriver échouait avec `webdriverUnreachable`. L’échec concerne le cycle arrêt/redémarrage de l’enveloppe dans WebDriver ; la gestion des processus de cette enveloppe est une cause possible non confirmée. Ce résultat historique ne valide pas le redémarrage de l’enveloppe AppImage finale, qui n’est pas déclaré réussi.

Le contenu de l’AppImage finale a été extrait et lancé par son `AppRun`. Avec le dernier contrôle du rendu, les dix étapes passent, dont le redémarrage, les deux cartes visibles et la capture. Les empreintes du binaire principal extrait et de son `AppRun` sont inchangées par rapport au premier artefact. Ce résultat valide le contenu distribué par l’AppImage dans cet environnement ; il reste distinct du redémarrage de l’enveloppe extérieure.

| Élément | SHA-256 |
| --- | --- |
| [library-manager_0.1.0_amd64.deb](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/library-manager_0.1.0_amd64.deb) | `79e810018b4c747354a92c4e3a214124ac0043d1133b2d884b430da9f04ef05f` |
| [library-manager-0.1.0-1.x86_64.rpm](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/library-manager-0.1.0-1.x86_64.rpm) | `2d45b3c41c0ef1692348412a526111a2cac39968d374fc26b5acf7974a578fbb` |
| [library-manager_0.1.0_amd64.AppImage](https://github.com/QrCommunication/library-manager/releases/download/v0.1.0/library-manager_0.1.0_amd64.AppImage) | `204390a19c180eefe5fab0e279e1ee7396f72a01766b4c3044ab2d526eafa959` |
| Binaire principal extrait de l’AppImage | `7054446b0de9b88f40a64470ec4453bb24edc7c34b694377f7f50d480313de4f` |
| Enveloppe `AppRun` extraite | `eb0b254ac0dae6543e6dd7cd02e1baf40a060de95b927313122d6b46323c00aa` |

### Livres, réseau et appareils

Une collection privée de 133 EPUB a servi au contrôle d’import. Les 133 empreintes SHA-256 des sources sont conservées. La réimportation des 133 fichiers produit 133 détections de doublon et aucune erreur. Les livres, leurs noms, leurs métadonnées privées et les fichiers du profil restent hors du dépôt et de la publication ; seuls ces nombres agrégés sont documentés.

La recherche Web anonyme a été exercée réellement, ainsi que le catalogue public z.ai, qui a retourné 21 modèles au moment de l’essai. Les autres adaptateurs ont été vérifiés avec leurs contrats et des réponses simulées. Aucun appel LLM payant n’a été effectué ; l’authentification, l’enrichissement et le chat avec les clés API de chaque fournisseur ne sont donc pas déclarés validés en conditions réelles. La liste des modèles est dynamique et peut changer après ce compte rendu. Voir [PROVIDERS.md](PROVIDERS.md).

Aucune écriture sur un appareil physique n’a été effectuée. La détection, l’indexation et les protocoles de transfert disposent de tests et de fixtures, mais la compatibilité avec toutes les liseuses, tous les firmwares Xteink/CrossPoint et tous les clients Calibre sans fil n’est pas établie par cette livraison. Voir [DEVICE_PROTOCOL.md](DEVICE_PROTOCOL.md).

### Sécurité et limites ouvertes

Les relevés locaux `pnpm audit` et `cargo audit` rapportent zéro vulnérabilité dans leur compteur de vulnérabilités. Cette mesure ne couvre pas les avertissements d’absence de maintenance ou d’intégrité mémoire, ni les alertes GitHub décrites ci-dessous. Elle ne constitue pas une validation de sécurité sans réserve.

| Sujet | État et portée |
| --- | --- |
| `glib 0.18.5` | [Dependabot nº 1](https://github.com/QrCommunication/library-manager/security/dependabot/1) reste ouverte, gravité moyenne. [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html) décrit une erreur d’intégrité mémoire dans `VariantStrIter`, corrigée à partir de 0.20.0. Le SDK GTK utilisé par Tauri dépend de la branche 0.18 ; imposer directement 0.20 ne constitue pas une mise à jour compatible de cette chaîne. Le traçage effectué n’a identifié aucun appel runtime à `VariantStrIter` dans le périmètre inspecté. Cette absence de chemin identifié ne supprime pas l’alerte transitive. |
| `proc-macro-error` | L’avertissement [RUSTSEC-2024-0370](https://rustsec.org/advisories/RUSTSEC-2024-0370.html) signale une dépendance de macros non maintenue. Aucun correctif amont n’est annoncé dans l’avis ; le suivi de la chaîne de dépendances reste nécessaire. |
| SHA-1 du protocole Calibre | [CodeQL nº 1](https://github.com/QrCommunication/library-manager/security/code-scanning/1), règle `rust/weak-sensitive-data-hashing`, reste ouverte et est classée élevée par la règle. Le [handshake Calibre](https://github.com/QrCommunication/library-manager/blob/8f4de47ab7800f69e3faf02c9d1fc10ea34b6b68/crates/library-core/src/calibre_wireless.rs#L750) utilise le challenge SHA-1 historique attendu par ce protocole. Ce réseau n’est pas protégé par TLS ; il doit rester sur un LAN de confiance. La compatibilité du protocole ne rend pas ce mécanisme équivalent à une authentification moderne. |
| Moteur MOBI en C | Le [moteur de conversion](https://github.com/QrCommunication/library-manager/blob/8f4de47ab7800f69e3faf02c9d1fc10ea34b6b68/crates/library-core/src/conversion.rs) lance le compagnon embarqué avec arguments structurés, environnement réduit, fichiers temporaires privés et limites de durée, de taille et de sortie. Il ne dispose pas d’une sandbox du système d’exploitation. Le processus borné réduit l’impact des fichiers problématiques, sans constituer une isolation complète du parseur C. |

Les protections applicatives inspectées comprennent les originaux immuables, les variantes publiées sans écrasement, les contrôles de chemins et liens symboliques, le lecteur EPUB sans scripts ni ressources distantes, et la séparation entre [recherche Web publique](../crates/library-core/src/web.rs) et clients authentifiés des fournisseurs. Les livres et les pages Web sont des données non fiables, jamais des autorisations d’exécuter des commandes ou de supprimer des fichiers. La [politique de sécurité](../SECURITY.md) décrit le signalement privé et ces frontières.

L’[analyse CodeQL de 3e6177b](https://github.com/QrCommunication/library-manager/actions/runs/37887628523) a terminé avec succès. Les deux alertes GitHub existantes, concernant glib et le challenge SHA-1 historique, restent ouvertes et visibles. Leur publication dans ce compte rendu, les tests réussis et l’absence d’appel identifié à un type concerné ne valent pas correction de ces alertes.
