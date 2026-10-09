# Qualité et validations de Library Manager 0.1.0

Compte rendu du 9 octobre 2026. Les paquets Linux, le moteur et l’interface ont été vérifiés dans les périmètres décrits ci-dessous. Les alertes de sécurité ouvertes, les limites du test AppImage et les fonctions sans essai matériel restent explicites.

## Sources et preuves

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

## Vérifications automatisées

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

## Parcours natif du DEB

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

## Installation et AppImage

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

## Livres, réseau et appareils

Une collection privée de 133 EPUB a servi au contrôle d’import. Les 133 empreintes SHA-256 des sources sont conservées. La réimportation des 133 fichiers produit 133 détections de doublon et aucune erreur. Les livres, leurs noms, leurs métadonnées privées et les fichiers du profil restent hors du dépôt et de la publication ; seuls ces nombres agrégés sont documentés.

La recherche Web anonyme a été exercée réellement, ainsi que le catalogue public z.ai, qui a retourné 21 modèles au moment de l’essai. Les autres adaptateurs ont été vérifiés avec leurs contrats et des réponses simulées. Aucun appel LLM payant n’a été effectué ; l’authentification, l’enrichissement et le chat avec les clés API de chaque fournisseur ne sont donc pas déclarés validés en conditions réelles. La liste des modèles est dynamique et peut changer après ce compte rendu. Voir [PROVIDERS.md](PROVIDERS.md).

Aucune écriture sur un appareil physique n’a été effectuée. La détection, l’indexation et les protocoles de transfert disposent de tests et de fixtures, mais la compatibilité avec toutes les liseuses, tous les firmwares Xteink/CrossPoint et tous les clients Calibre sans fil n’est pas établie par cette livraison. Voir [DEVICE_PROTOCOL.md](DEVICE_PROTOCOL.md).

## Sécurité et limites ouvertes

Les relevés locaux `pnpm audit` et `cargo audit` rapportent zéro vulnérabilité dans leur compteur de vulnérabilités. Cette mesure ne couvre pas les avertissements d’absence de maintenance ou d’intégrité mémoire, ni les alertes GitHub décrites ci-dessous. Elle ne constitue pas une validation de sécurité sans réserve.

| Sujet | État et portée |
| --- | --- |
| `glib 0.18.5` | [Dependabot nº 1](https://github.com/QrCommunication/library-manager/security/dependabot/1) reste ouverte, gravité moyenne. [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html) décrit une erreur d’intégrité mémoire dans `VariantStrIter`, corrigée à partir de 0.20.0. Le SDK GTK utilisé par Tauri dépend de la branche 0.18 ; imposer directement 0.20 ne constitue pas une mise à jour compatible de cette chaîne. Le traçage effectué n’a identifié aucun appel runtime à `VariantStrIter` dans le périmètre inspecté. Cette absence de chemin identifié ne supprime pas l’alerte transitive. |
| `proc-macro-error` | L’avertissement [RUSTSEC-2024-0370](https://rustsec.org/advisories/RUSTSEC-2024-0370.html) signale une dépendance de macros non maintenue. Aucun correctif amont n’est annoncé dans l’avis ; le suivi de la chaîne de dépendances reste nécessaire. |
| SHA-1 du protocole Calibre | [CodeQL nº 1](https://github.com/QrCommunication/library-manager/security/code-scanning/1), règle `rust/weak-sensitive-data-hashing`, reste ouverte et est classée élevée par la règle. Le [handshake Calibre](https://github.com/QrCommunication/library-manager/blob/8f4de47ab7800f69e3faf02c9d1fc10ea34b6b68/crates/library-core/src/calibre_wireless.rs#L750) utilise le challenge SHA-1 historique attendu par ce protocole. Ce réseau n’est pas protégé par TLS ; il doit rester sur un LAN de confiance. La compatibilité du protocole ne rend pas ce mécanisme équivalent à une authentification moderne. |
| Moteur MOBI en C | Le [moteur de conversion](https://github.com/QrCommunication/library-manager/blob/8f4de47ab7800f69e3faf02c9d1fc10ea34b6b68/crates/library-core/src/conversion.rs) lance le compagnon embarqué avec arguments structurés, environnement réduit, fichiers temporaires privés et limites de durée, de taille et de sortie. Il ne dispose pas d’une sandbox du système d’exploitation. Le processus borné réduit l’impact des fichiers problématiques, sans constituer une isolation complète du parseur C. |

Les protections applicatives inspectées comprennent les originaux immuables, les variantes publiées sans écrasement, les contrôles de chemins et liens symboliques, le lecteur EPUB sans scripts ni ressources distantes, et la séparation entre [recherche Web publique](../crates/library-core/src/web.rs) et clients authentifiés des fournisseurs. Les livres et les pages Web sont des données non fiables, jamais des autorisations d’exécuter des commandes ou de supprimer des fichiers. La [politique de sécurité](../SECURITY.md) décrit le signalement privé et ces frontières.

L’[analyse CodeQL de 3e6177b](https://github.com/QrCommunication/library-manager/actions/runs/37887628523) a terminé avec succès. Les deux alertes GitHub existantes, concernant glib et le challenge SHA-1 historique, restent ouvertes et visibles. Leur publication dans ce compte rendu, les tests réussis et l’absence d’appel identifié à un type concerné ne valent pas correction de ces alertes.
