# Library Manager

<img src="public/library-manager.png" width="96" height="96" alt="Library Manager" />

**Votre bibliothèque, partout où vous lisez.** Application autonome pour organiser les livres numériques, lire les EPUB et préparer des fichiers adaptés aux liseuses. Ce document décrit la version **0.2.1**.

[English](README.en.md) · [Téléchargements](https://github.com/QrCommunication/library-manager/releases) · [Guide utilisateur](docs/USER_GUIDE.md) · [Architecture](docs/BLUEPRINT.md) · [Signaler un problème](https://github.com/QrCommunication/library-manager/issues)

Library Manager est développé par QR Communication, sous licence **GPL-3.0**. Son moteur Rust, sa base SQLite et son convertisseur MOBI sont embarqués. **Calibre, Node et Python ne sont pas nécessaires pour utiliser les paquets de l’application.**

## Bibliothèque

- Affichage par couvertures ou tableau, regroupement par auteur, série ou genre.
- Recherche dans les titres, auteurs, séries, genres, descriptions, ISBN, éditeurs et notes personnelles, avec gestion des accents.
- Filtres combinables : auteur, série, genre, étiquette, langue, format, état de lecture, favori, métadonnées à vérifier, couverture manquante, taille et présence sur un appareil.
- Tri par titre, auteur, série et numéro, date d’ajout, modification, taille, progression, publication ou note.
- Notes personnelles, favoris, évaluations, progression et variantes d’un même livre.
- Originaux conservés intacts ; fichiers normalisés rangés selon **Auteur → Série → numéro et titre**. Le tome zéro et les numéros décimaux sont pris en charge.

Les actions groupées utilisent la même barre de sélection dans la bibliothèque et les vues consacrées aux livres des appareils : **Assistant**, **Vérifier les métadonnées**, **Envoyer à la liseuse** et **Retirer de la bibliothèque**. Une sélection peut contenir jusqu’à 200 livres. Les actions indisponibles indiquent leur condition, par exemple un fournisseur actif pour l’analyse ou un appareil accessible en écriture pour le transfert.

**Retirer de la bibliothèque** enlève les livres du catalogue local après confirmation. Cette action conserve les fichiers originaux, les variantes et les copies présentes sur les appareils. Le retrait est enregistré dans l’historique et peut être annulé ; la restauration vérifie les fichiers conservés et les éventuels conflits avec le catalogue courant.

## Assistant et métadonnées

Les imports déclenchent automatiquement l’analyse et l’enrichissement lorsque l’option est activée. Sans fournisseur configuré, les livres restent utilisables et les traitements attendent la configuration.

Six fournisseurs sont proposés : **Z.ai, Kimi, MiniMax, Codex via l’API OpenAI, Claude et Mistral**. Les modèles sont récupérés à la demande depuis l’API du fournisseur ou son catalogue officiel, avec date et provenance. Z.ai utilise son catalogue officiel public lorsque l’API ne propose pas d’endpoint de liste.

L’accès au Web passe par les outils de Library Manager, communs aux fournisseurs. Les sources bibliographiques accompagnent les propositions. Les corrections incertaines restent à vérifier ; un modèle ne reçoit aucun outil de suppression ou d’exécution de commande. Les notes personnelles et la progression ne sont pas des métadonnées à réécrire par l’IA.

Dans la **Bibliothèque**, cochez les livres concernés : **Vérifier les métadonnées** lance leurs analyses, ou **Assistant** ouvre une conversation sur cette sélection. L’analyse groupée exige un fournisseur sélectionné, configuré et prêt, ainsi qu’un modèle renseigné. Les livres dont une analyse est déjà active ne sont pas ajoutés une seconde fois au lot.

L’assistant peut inspecter les fichiers et demander des analyses. L’option **Autoriser l’assistant à modifier et organiser les livres cochés** s’applique uniquement à la demande envoyée et aux livres sélectionnés ; elle est désactivée après l’acceptation de cette demande. Sans cette autorisation, il reste en lecture seule. Les originaux sont conservés.

Cliquez sur **Examiner les propositions**, ou sur **Examiner** auprès d’un livre ayant une proposition disponible, pour ouvrir sa fenêtre de revue. Comparez les valeurs actuelles, les changements proposés et leurs sources, puis appliquez la proposition. Le badge « à vérifier » peut aussi signaler des métadonnées incomplètes sans proposition à examiner.

La validation d’une proposition est enregistrée durablement avec la modification du livre et son historique : une proposition appliquée ne revient pas après un rafraîchissement ou un redémarrage. Une édition personnelle ne valide pas implicitement une proposition. Les champs déjà identiques ne sont pas présentés comme des changements ; une proposition devenue obsolète ne peut pas écraser silencieusement une édition plus récente. Les opérations réversibles peuvent être annulées depuis l’historique.

L’inspection lit le fichier réellement conservé et vérifie son intégrité ; les extraits EPUB donnent priorité aux pages de titre, de copyright et d’édition, puis à un chapitre. Cette lecture est bornée et ne constitue pas une lecture intégrale du livre.

Les fournisseurs nécessitent une clé API et peuvent facturer leur utilisation. Un abonnement à un site de chat n’est pas automatiquement une clé API. Choisissez un modèle et une clé dans les paramètres. Les clés peuvent rester en mémoire pour la session ou être conservées dans le coffre de secrets Linux ; aucun repli silencieux en texte clair.

Voir [les fournisseurs et leurs sources officielles](docs/PROVIDERS.md) et [la politique de métadonnées](docs/METADATA_POLICY.md).

## Liseuses et optimisation

Les volumes USB/SD montés sont détectés et indexés. Les livres déjà présents sont mis en évidence et peuvent être filtrés. La déconnexion retire immédiatement cette présence du catalogue courant. Les appareils MTP sont accessibles lorsqu’ils sont déjà montés par le bureau Linux via GVfs.

La barre de progression de l’inventaire USB suit la quantité de données réellement lue. Les livres détectés apparaissent au fil de l’inventaire. Ceux qui sont présents uniquement sur la liseuse sont signalés dans la bibliothèque : importez un livre ou tous les livres absents du catalogue local, sans modifier les fichiers sur la carte. Le résultat indique les imports réussis, les doublons et les fichiers refusés.

Le connecteur **CrossPoint** utilise directement le transfert HTTP du firmware sur le réseau local. Activez le mode de transfert sur la liseuse puis indiquez son adresse dans l’application. L’installation de Calibre n’est pas nécessaire. Les copies sont vérifiées et les fichiers déjà présents ne sont pas écrasés implicitement.

Pour envoyer plusieurs livres, cochez-les puis choisissez **Envoyer à la liseuse**. La fenêtre de transfert permet de choisir un appareil connecté accessible en écriture et un profil d’optimisation, puis de confirmer l’envoi. Le traitement apparaît dans l’activité ; les fichiers de la bibliothèque locale restent conservés.

Un serveur **Calibre sans fil intégré** permet aussi de connecter un client compatible tel que le module Calibre de KOReader. Il s’active explicitement dans le panneau Liseuses, sur l’adresse locale du PC et le port 9090. Cette version utilise une connexion configurée par adresse IP ; la découverte UDP et les essais matériels de ce protocole sans fil ne sont pas encore validés. Le protocole reste autonome, sans installer Calibre.

| Profil EPUB | Usage |
| --- | --- |
| Sans perte | Compression ZIP renforcée, contenu conservé. |
| Équilibré | Réduction prudente des grandes images. |
| Xteink | Images adaptées au petit écran, niveaux de gris et polices embarquées retirées lorsque cela reste sûr. |
| Texte seul | Images et polices retirées lorsque possible ; textes, légendes et descriptions conservés. |

Une optimisation produit une variante et un rapport : taille avant/après, images et polices modifiées, avertissements et contrôle du texte et des chapitres. Vous pouvez optimiser un livre séparément ou avant l’envoi.

Voir [les protocoles et limites des appareils](docs/DEVICE_PROTOCOL.md).

## Formats et lecture

| Format | Catalogue | Lecture intégrée | Conversion |
| --- | --- | --- | --- |
| EPUB | Oui | Oui | EPUB, TXT, HTML, FB2, MOBI6 |
| TXT, HTML, FB2 | Oui | Via une variante EPUB | EPUB, TXT, HTML, FB2, MOBI6 |
| MOBI, AZW3/KF8 sans DRM | Oui | Via une variante EPUB | EPUB, TXT, HTML, FB2, MOBI6 |
| PDF, CBZ | Oui | Pas de lecteur intégré pour ces formats | Conservation du fichier original |

La conversion de documents à mise en page complexe peut aplatir le style. Les avertissements sont affichés ; l’application ne promet pas une reproduction identique de PDF, bandes dessinées ou contenus interactifs. Les fichiers protégés par DRM ne sont pas déchiffrés.

Le lecteur EPUB propose un sommaire et une position sauvegardée. Le contenu du livre est assaini et isolé, avec scripts et ressources distantes bloqués.

## Télécharger 0.2.1

Consultez la [page de livraison v0.2.1](https://github.com/QrCommunication/library-manager/releases/tag/v0.2.1) pour les fichiers publiés, puis choisissez le paquet correspondant à votre système et à son architecture.

| Plateforme | Noms des paquets 0.2.1 |
| --- | --- |
| Linux x86_64 (64 bits) | `library-manager_0.2.1_amd64.deb` · `library-manager-0.2.1-1.x86_64.rpm` · `library-manager_0.2.1_amd64.AppImage` |
| Windows 11 x64 | `library-manager_0.2.1_windows_x64_unsigned.msi` · `library-manager_0.2.1_windows_x64_unsigned_setup.exe` |
| macOS 13 ou ultérieur, Apple Silicon | `library-manager_0.2.1_macos_arm64_signed.dmg` · `library-manager_0.2.1_macos_arm64_signed.app.zip` |
| macOS 13 ou ultérieur, Intel | `library-manager_0.2.1_macos_x64_signed.dmg` · `library-manager_0.2.1_macos_x64_signed.app.zip` |

Avant l’installation, vérifiez l’empreinte du téléchargement dans **SHA256SUMS**. Consultez **BUILD_MANIFEST.json** et les rapports joints à la page de livraison pour le commit source, la provenance des paquets et les contrôles réalisés. Les notes de version précisent le périmètre des essais.

Les installateurs Windows MSI et EXE n’ont pas de signature Authenticode. Pour macOS, la publication exige une signature Developer ID, une notarisation **Accepted**, un ticket agrafé aux paquets et une validation Gatekeeper. Vérifiez ces résultats dans les rapports de notarisation de chaque architecture.

Les paquets Linux utilisent Ubuntu 22.04, avec glibc 2.35, comme base de compilation. Après le téléchargement, utilisez la commande correspondant à votre paquet.

Debian, Ubuntu et dérivés :

```sh
sudo apt install ./library-manager_0.2.1_amd64.deb
```

Fedora et distributions RPM :

```sh
sudo dnf install ./library-manager-0.2.1-1.x86_64.rpm
```

Le gestionnaire de paquets installe les bibliothèques système GTK/WebKit, leurs dépendances et le magasin de certificats TLS `ca-certificates`, nécessaire aux connexions HTTPS. Après installation, lancez **Library Manager** depuis le menu des applications. Aucun environnement Node, Python, Rust ou Calibre n’est requis.

AppImage, sans installation du paquet :

```sh
chmod +x ./library-manager_0.2.1_amd64.AppImage
./library-manager_0.2.1_amd64.AppImage
```

L’AppImage utilise les bibliothèques compatibles et le magasin de certificats du système. Son montage classique nécessite FUSE. Si FUSE n’est pas disponible, essayez le mode d’extraction et d’exécution :

```sh
APPIMAGE_EXTRACT_AND_RUN=1 ./library-manager_0.2.1_amd64.AppImage
```

### Windows

Téléchargez l’installateur **EXE x64** ou **MSI x64**, puis lancez-le. L’EXE installe l’application pour l’utilisateur courant ; le MSI l’installe pour la machine et demande des droits administrateur. Vérifiez la provenance et le SHA-256 de l’installateur sur la page de livraison.

Si **Microsoft Edge WebView2 Runtime** manque, l’installateur télécharge son bootstrapper et lance son installation silencieuse. Une connexion Internet est alors nécessaire ; ces paquets ne sont pas des installateurs entièrement hors ligne. Lancez ensuite **Library Manager** depuis le menu Démarrer. Voir les [options d’installation Windows de Tauri](https://v2.tauri.app/distribute/windows-installer/).

### macOS

**macOS 13 ou ultérieur** est requis. Choisissez **ARM64** pour un Mac Apple Silicon, ou **x86_64** pour un Mac Intel ; les deux applications sont distinctes. Après le téléchargement, ouvrez le **DMG**, glissez **Library Manager** dans **Applications**, puis lancez-le depuis ce dossier. Le fichier **`.app.zip`** constitue une alternative : décompressez-le et placez l’application obtenue dans **Applications**.

Consultez le rapport de notarisation correspondant à votre architecture sur la page de livraison avant l’installation.

### Mise à jour

Fermez Library Manager : la fermeture de son unique fenêtre quitte l’application, sans la laisser active dans une icône de notification. Téléchargez le nouveau paquet pour la même plateforme et la même architecture. Sous Linux ou Windows, installez-le avec le même format de paquet ; sous macOS, remplacez l’application dans **Applications**. Pour une AppImage, remplacez le fichier utilisé pour la lancer.

Rouvrez l’application et vérifiez la version dans **Paramètres → À propos**, également affichée en bas de la barre latérale. Après installation du paquet 0.2.1, elle doit indiquer **0.2.1**. Cette mise à jour remplace l’application ; il n’est pas nécessaire de retirer les livres du catalogue ni de supprimer son profil local.

Consultez le [rapport de qualité](docs/QUALITY.md) pour les vérifications effectuées et les limites des essais.

## Français, anglais et autres langues

La langue suit le système par défaut et peut être choisie dans les paramètres. Pour ajouter une langue, traduisez un fichier JSON dans `src/lib/locales/` : le sélecteur le découvre automatiquement. Voir [LOCALIZATION.md](docs/LOCALIZATION.md).

## Développement

Stack : **Tauri 2, Rust, Svelte 5, TypeScript et SQLite embarqué**. Les versions de compilation sont fixées dans les manifestes et les lockfiles. Le noyau métier ne dépend pas de la WebView.

```sh
pnpm install --frozen-lockfile --force --ignore-scripts
pnpm tauri dev
```

Consultez [CONTRIBUTING.md](CONTRIBUTING.md) pour les dépendances natives, les checks et les règles de contribution. Le mode navigateur `?demo=1` présente des livres fictifs et ne manipule aucun fichier, appareil ou fournisseur réel. L’application native utilise les services réels.

## Données et sécurité

La bibliothèque et les conversations sont stockées localement. Les requêtes IA envoient au fournisseur configuré les informations et extraits utiles à la demande ; elles ne sont pas un mode hors ligne. Vous pouvez désactiver l’enrichissement automatique ou le Web dans les paramètres.

Ne joignez jamais de clé API ou de livre privé à un ticket. Voir [SECURITY.md](SECURITY.md). La licence de libmobi et les informations de redistribution sont dans [THIRD_PARTY.md](docs/THIRD_PARTY.md).

Les tests automatisés, la validation du paquet et les essais sur appareil physique sont des preuves différentes. Les notes de version précisent leur périmètre.
