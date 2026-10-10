# Livraison 0.2.1

Demande : validation persistante des propositions, actions communes sur toutes les vues de bibliothèque, installateurs Linux/macOS/Windows et notarisation macOS.

## État courant

Le dernier checkpoint poussé est [8e37f8a87cca647b302d36c4b97749e186d968b8](https://github.com/QrCommunication/library-manager/commit/8e37f8a87cca647b302d36c4b97749e186d968b8). La version des manifestes est **0.2.1** ; sa livraison reste en préparation. Le lien statique Windows, les notices multiplateformes, les déclencheurs CI et le workflow de publication contrôlée sont poussés. Les derniers correctifs de l’adaptateur Windows natif et de l’environnement de notarisation Apple attendent leur nouvelle validation native. Aucun tag de livraison n’est encore créé et le workflow de release n’a pas encore été exécuté.

- Revue persistante et publication atomique : raccords Library/Manager/IPC/UI implémentés. La publication du worker enregistre livre, références de variantes, historique, proposition/revue et tâche terminée dans une transaction commune ; les notifications suivent la libération des verrous.
- Actions groupées : barre commune raccordée aux vues Bibliothèque/Appareils/Assistant, jusqu’à 200 livres, avec disponibilité du fournisseur, confirmation du transfert et retrait réversible du catalogue. Le retrait conserve les fichiers physiques et possède un reçu idempotent.
- Portage : façade sécurisée Unix/Windows, stockage et base de données intégrés. Tests ciblés réussis : Database 12, secure_fs 8, Storage 12, Transfer 11, Jobs 20, Repository 31, Library 29, Manager 29, Conversion 12, Devices 19, inventaire EPUB 5, Enrichment 23 et Settings 4. Clippy core toutes cibles réussi sur l’hôte Linux. Ces comptes ne constituent pas une suite globale additionnable.
- Interface : 53 tests réussis, contrôle TypeScript/Svelte sans erreur ni avertissement. Les revues indépendantes ciblées rapportent `findings: []` dans leurs périmètres inspectés.
- Validation globale du snapshot `69641a0` : 315 tests core et huit tests Tauri réussis, avec un test réseau volontairement ignoré ; formatage et Clippy réussis. Les 53 tests frontend et le contrôle TypeScript/Svelte passent. La [CI Linux 38025145928](https://github.com/QrCommunication/library-manager/actions/runs/38025145928) est réussie, puis la [CI Linux du checkpoint `71adedd`](https://github.com/QrCommunication/library-manager/actions/runs/38027209924) passe également. Les échecs de DLL zlib et de notices de la [CI native `69641a0`](https://github.com/QrCommunication/library-manager/actions/runs/38025145921) sont des résultats historiques ; les correctifs poussés et les obstacles natifs actuels sont détaillés ci-dessous.
- Cache des propositions : la baseline native R4 du paquet CI `e351629` reproduisait la disparition du bouton de revue après une édition personnelle, malgré une proposition correctement réancrée en base. Le scénario étendu R5 passe désormais sur le binaire CI `69641a0`, avec les autres parcours natifs consignés ci-dessous. Ces résultats devront être repris sur le binaire du dernier commit de livraison.
- Windows natif `8e37f8a` : compilation du sidecar C statique réussie ; 118 tests core passent, 194 échouent et un test est ignoré. Les causes communes identifiées sont la réouverture du répertoire pour flush (OS 5) et la publication par renommage relatif (OS 87). Le correctif NT est revu (`findings: []`) ; les 12 tests de l’adaptateur compilent pour Windows GNU et Clippy passe sans avertissement. Leur exécution native corrigée reste attendue.
- Apple : les deux runners macOS réussissent chacun 316 tests core et six tests Tauri, puis signent leurs paquets. La notarisation échoue avec HTTP 401 : des variables `APPLE_ID`/`APPLE_PASSWORD` présentes mais vides prennent priorité dans Tauri 2.12.1. Les identifiants centraux répondent au contrôle d’authentification HTTP 200 ; le correctif d’environnement du workflow est implémenté et sa fixture synthétique réussit dans les deux modes ; la relance native est attendue. Les sept secrets nécessaires sont configurés, mais notarisation `Accepted`, tickets agrafés et vérifications Gatekeeper de l’application et du DMG restent à prouver sur les artefacts finaux.
- Publication : release, archives exactes, téléchargements publics et correspondance des sommes SHA-256 restent à effectuer après validation. La documentation utilisateur et les contrats sont synchronisés ; voir [QUALITY.md](QUALITY.md), [USER_GUIDE.md](USER_GUIDE.md) et [BUILD.md](BUILD.md).

## Paquets CI exécutés et correctifs en attente

Les quatre parcours suivants ont été exécutés avec le binaire Linux du paquet CI `69641a0`, version 0.2.1, SHA-256 `abc65b87bc671477f9b0418d0ed064b0f232074913d4c4e309c40304e9c341a1` :

| Parcours natif | Résultat |
| --- | --- |
| Actions du catalogue, scénario étendu R5 | 9 contrôles réussis |
| Parcours général | 10 contrôles réussis |
| Assistant | 10 contrôles réussis |
| Transfert CrossPoint synthétique | 4 contrôles réussis |

Le paquet AppImage du même commit `69641a0` a également réussi dix contrôles généraux via son `AppRun` extrait, sur un autre profil synthétique sans réseau. Son lanceur a pour SHA-256 `eb0b254ac0dae6543e6dd7cd02e1baf40a060de95b927313122d6b46323c00aa`. Le payload ELF AppImage a une empreinte distincte de celle du DEB, notamment à cause du traitement de ses chemins de chargement ; le workflow contrôle chaque paquet avec sa propre provenance. Ce passage ne prouve pas l’exécution de l’AppImage avec FUSE.

Le parcours CrossPoint utilise la vraie interface, les IPC publics et un serveur HTTP synthétique sur une adresse privée au port 80, dans un réseau Docker interne sans sortie Internet. Il valide le reçu actif dans Liseuses avant le résultat public de la tâche, sa mise à jour après complétion, sa conservation après navigation aller-retour et l’égalité exacte des octets/SHA du fichier publié avec l’original synthétique. Rapport local : `/tmp/library-manager-0.2.1-qa/crosspoint-receipt-021-r4.json`. Aucun endpoint réservé aux tests ni monkeypatch de l’application n’est utilisé. Ces parcours utilisent des profils synthétiques ; ils ne prouvent ni transfert sur une liseuse physique ni appel fournisseur payant.

Correctifs poussés et dernières validations à obtenir :

- **Lien Windows** : `--enable-tools-static` produit une option Libtool `-static`, qui ne suffit pas pour les dépendances externes. Le build poussé transmet `TOOLS_STATIC=-all-static` et `LIBZ_LDFLAGS=-L/ucrt64/lib -lz`. Le nom de bibliothèque permet à Libtool de conserver sa dépendance zlib et de lier l’archive au bon endroit ; passer directement l’archive dans les flags produisait une archive imbriquée et une référence `uncompress` non résolue. Les trois tests de `build.rs` passent, dont une reproduction réelle de la dépendance transitive. La compilation C du runner Windows `8e37f8a` confirme ce correctif ; l’audit des DLL non système reste strict.
- **Adaptateur Windows** : la publication utilise désormais `NtSetInformationFile(FileRenameInformation)` avec parent épinglé, contrôle d’identité et interdiction de remplacement. La synchronisation ouvre le répertoire lui-même par un nom NT vide relatif au handle, vérifie son identité et demande le vrai flush natif. Aucun assouplissement des ACL ou des protections contre les liens, aucun test ignoré et aucune barrière factice. Revue et compilation statique réussies ; validation native en attente.
- **Notices** : le générateur et les contrôles poussés passent 13 autotests. Le catalogue canonique couvre 701 paquets et 67 limitations documentées. La concordance des notices embarquées dans chaque paquet final devra être vérifiée par la CI et le workflow de livraison.
- **Déclencheurs** : les pushes documentaires sur `main` déclenchent les builds natifs. Le dernier commit contenant README, documentation et correctifs doit disposer de ses propres artefacts Linux, Windows et macOS avant le tag ; les artefacts du snapshot précédent ne suffisent pas.
- **Notarisation** : retirer du processus Tauri les variables d’authentification Apple vides afin qu’il utilise les identifiants API complets. Le contrôle central HTTP 200 valide l’authentification ; il ne remplace pas la soumission `Accepted`, l’agrafage et Gatekeeper.

## Conditions avant publication

Le workflow de release est poussé, revu et prêt à être déclenché par le tag ; il n’a pas encore été exécuté. Il exige le commit exact du tag sur `main`, les CI Linux et des trois runners natifs réussies, ainsi que CodeQL réussi pour le même commit. Il contrôle les licences et la provenance des paquets, les preuves Apple `Accepted`/agrafage/Gatekeeper et le rapport Windows explicitement non signé Authenticode. Il exécute ensuite les parcours natifs sur le vrai paquet DEB et l’AppRun extrait, dans des profils synthétiques sans réseau. Les artefacts seront d’abord contrôlés dans une release brouillon, avec manifeste et sommes SHA-256 concordants ; après publication, les téléchargements anonymes et leurs empreintes seront vérifiés avant promotion comme dernière version. Aucun tag ni publication ne doit précéder la validation du commit final. Les signatures macOS du checkpoint ne suffisent pas à établir sa notarisation.

## Contrats

Revue : enveloppe résultat `{proposal,review}` ; états pending/applied/dismissed/obsolete. La transaction CAS commune écrit livre, fichiers, revue et historique ; une édition personnelle ne consomme pas la proposition. Les propositions legacy restent lisibles et peuvent être traitées explicitement sans révision source fictive.

Sélection : au maximum 200 IDs distincts. Analyse disponible uniquement si fournisseur reconnu/configuré/prêt et modèle renseigné. Le transfert exige une cible connectée accessible en écriture et une validation explicite.

Retrait : `books_remove({requestId,books:[{bookId,expectedRevision}]})`. Retire du catalogue local, conserve originaux/variantes/copies appareil. Journal catalogueRemove réversible et receipt idempotent ; undo vérifie fichiers et collisions.

## Preuves de livraison

Séparer tests source, exécution native Linux, builds Windows/macOS, signature, notarisation Accepted/stapler/Gatekeeper et téléchargements publics. Ne pas attribuer à Windows 11 physique un test exécuté sur runner Windows Server. Ne pas publier de capture contenant la bibliothèque personnelle.

## Historique des checkpoints et validations intermédiaires

Les checkpoints plateforme `6b57b05` et `efd58c7`, puis documentaire `8b81301`, précèdent le commit courant. Les résultats ci-dessous portent sur leurs snapshots intermédiaires et ne remplacent pas la validation finale.

Clippy workspace sur le snapshot fonctionnel Linux réussi. À ce stade, les ports avaient les résultats suivants : secure_fs 7, storage 12, transfer 11, manager 26, library 24, conversion 12, inventaire EPUB 5 et devices 19 tests ciblés réussis ; Clippy core toutes cibles réussi. Ces contrôles ne remplacent pas la suite globale finale. Adaptateur Windows cross-compilé et Clippy GNU Windows réussi, sans exécution Windows encore.

Binaire local fonctionnel antérieur au portage : sept contrôles natifs réussis sur profil synthétique réseau isolé (huit vues, disponibilité IA, résolution durable après redémarrage, retrait confirmé, replay idempotent, conservation des octets). Le contrôle undo avait été interrompu par `webdriverUnreachable` ; le passage R3 ci-dessous a ensuite validé le pilotage corrigé. Aucun appel API payant ni écriture appareil.

Natif R3 (profil neuf, network=none) : 8/8 contrôles réussis, dont retrait/replay après redémarrage et undo UI 0→2 avec fichiers/octets préservés. Rapport local `/tmp/library-manager-0-2-1-qa-r3/native-catalogue-report.json`, binaire SHA256 `004a5c9c23cc21040068d0abaf9c339f34096a0af77e380cd073ebac95c34056`. Snapshot fonctionnel antérieur aux ports FS ; à reprendre sur le paquet CI final. Aucun retry transport utilisé au passage réussi.

Suite globale Linux après portage : 295 tests core réussis, un test réseau volontairement ignoré ; workspace build réussi. Ce résultat précède les derniers changements de publication atomique et de cache. Les validations encore en cours à ce checkpoint sont consignées ici comme historique ; leur état courant figure en tête de ce document.
