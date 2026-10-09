# Library Manager

<img src="public/library-manager.png" width="96" height="96" alt="Library Manager" />

**Votre bibliothèque, partout où vous lisez.** Application Linux autonome pour organiser les livres numériques, lire les EPUB et préparer des fichiers adaptés aux liseuses.

[English](README.en.md) · [Téléchargements](https://github.com/QrCommunication/library-manager/releases) · [Documentation](docs/BLUEPRINT.md) · [Signaler un problème](https://github.com/QrCommunication/library-manager/issues)

Library Manager est développé par QR Communication, sous licence **GPL-3.0**. Son moteur Rust, sa base SQLite et son convertisseur MOBI sont embarqués. **Calibre, Node et Python ne sont pas nécessaires pour utiliser les paquets de l’application.**

## Bibliothèque

- Affichage par couvertures ou tableau, regroupement par auteur, série ou genre.
- Recherche dans les titres, auteurs, séries, genres, descriptions, ISBN, éditeurs et notes personnelles, avec gestion des accents.
- Filtres combinables : auteur, série, genre, étiquette, langue, format, état de lecture, favori, métadonnées à vérifier, couverture manquante, taille et présence sur un appareil.
- Tri par titre, auteur, série et numéro, date d’ajout, modification, taille, progression, publication ou note.
- Notes personnelles, favoris, évaluations, progression et variantes d’un même livre.
- Originaux conservés intacts ; fichiers normalisés rangés selon **Auteur → Série → numéro et titre**. Le tome zéro et les numéros décimaux sont pris en charge.

## Assistant et métadonnées

Les imports déclenchent automatiquement l’analyse et l’enrichissement lorsque l’option est activée. Sans fournisseur configuré, les livres restent utilisables et les traitements attendent la configuration.

Six fournisseurs sont proposés : **Z.ai, Kimi, MiniMax, Codex via l’API OpenAI, Claude et Mistral**. Les modèles sont récupérés à la demande depuis l’API du fournisseur ou son catalogue officiel, avec date et provenance. Z.ai utilise son catalogue officiel public lorsque l’API ne propose pas d’endpoint de liste.

L’accès au Web passe par les outils de Library Manager, communs aux fournisseurs. Les sources bibliographiques accompagnent les propositions. Les corrections incertaines restent à vérifier ; un modèle ne reçoit aucun outil de suppression ou d’exécution de commande. Les notes personnelles et la progression ne sont pas des métadonnées à réécrire par l’IA.

Les fournisseurs nécessitent une clé API et peuvent facturer leur utilisation. Un abonnement à un site de chat n’est pas automatiquement une clé API. Choisissez un modèle et une clé dans les paramètres. Les clés peuvent rester en mémoire pour la session ou être conservées dans le coffre de secrets Linux ; aucun repli silencieux en texte clair.

Voir [les fournisseurs et leurs sources officielles](docs/PROVIDERS.md) et [la politique de métadonnées](docs/METADATA_POLICY.md).

## Liseuses et optimisation

Les volumes USB/SD montés sont détectés et indexés. Les livres déjà présents sont mis en évidence et peuvent être filtrés. La déconnexion retire immédiatement cette présence du catalogue courant. Les appareils MTP sont accessibles lorsqu’ils sont déjà montés par le bureau Linux via GVfs.

Le connecteur **CrossPoint** utilise directement le transfert HTTP du firmware sur le réseau local. Activez le mode de transfert sur la liseuse puis indiquez son adresse dans l’application. L’installation de Calibre n’est pas nécessaire. Les copies sont vérifiées et les fichiers déjà présents ne sont pas écrasés implicitement.

Un serveur **Calibre sans fil intégré** permet aussi de connecter un client compatible tel que le module Calibre de KOReader. Il s’active explicitement dans le panneau Liseuses, sur l’adresse locale du PC et le port 9090. Cette version utilise une connexion configurée par adresse IP ; la découverte UDP et les essais sur appareil physique ne sont pas encore validés. Le protocole reste autonome, sans installer Calibre.

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

## Installation

Les paquets **DEB et RPM** sont publiés dans les [versions GitHub](https://github.com/QrCommunication/library-manager/releases). Consultez les notes de la version pour l’architecture, les distributions testées et les sommes SHA-256.

Debian, Ubuntu et dérivés :

```sh
sudo apt install ./paquet.deb
```

Fedora et distributions RPM :

```sh
sudo dnf install ./paquet.rpm
```

Remplacez le nom par celui du paquet téléchargé. Le gestionnaire de paquets installe les bibliothèques système GTK/WebKit nécessaires. Après installation, lancez **Library Manager** depuis le menu des applications.

## Français, anglais et autres langues

La langue suit le système par défaut et peut être choisie dans les paramètres. Pour ajouter une langue, traduisez un fichier JSON dans `src/lib/locales/` : le sélecteur le découvre automatiquement. Voir [LOCALIZATION.md](docs/LOCALIZATION.md).

## Développement

Stack : **Tauri 2, Rust, Svelte 5, TypeScript et SQLite embarqué**. Les versions de compilation sont fixées dans les manifestes et les lockfiles. Le noyau métier ne dépend pas de la WebView.

```sh
pnpm install --frozen-lockfile
pnpm tauri dev
```

Consultez [CONTRIBUTING.md](CONTRIBUTING.md) pour les dépendances natives, les checks et les règles de contribution. Le mode navigateur `?demo=1` présente des livres fictifs et ne manipule aucun fichier, appareil ou fournisseur réel. L’application native utilise les services réels.

## Données et sécurité

La bibliothèque et les conversations sont stockées localement. Les requêtes IA envoient au fournisseur configuré les informations et extraits utiles à la demande ; elles ne sont pas un mode hors ligne. Vous pouvez désactiver l’enrichissement automatique ou le Web dans les paramètres.

Ne joignez jamais de clé API ou de livre privé à un ticket. Voir [SECURITY.md](SECURITY.md). La licence de libmobi et les informations de redistribution sont dans [THIRD_PARTY.md](docs/THIRD_PARTY.md).

Les tests automatisés, la validation du paquet et les essais sur appareil physique sont des preuves différentes. Les notes de version précisent leur périmètre.
