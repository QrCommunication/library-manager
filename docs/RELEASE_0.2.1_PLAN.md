# Livraison 0.2.1

Demande : validation persistante des propositions, actions communes sur toutes les vues de bibliothèque, installateurs Linux/macOS/Windows et notarisation macOS.

## Protocole de publication et preuves finales

La livraison 0.2.1 est vérifiée à partir du commit exact du tag, déjà poussé sur `main`. Les archives jointes à la release, `BUILD_MANIFEST.json`, `SHA256SUMS`, les rapports natifs et les rapports de signature/notarisation constituent les preuves finales. Les checkpoints ci-dessous documentent le travail de validation ; ils ne revendiquent pas une publication et ne remplacent pas les résultats du commit livré.

Les [README français](../README.md) et [anglais](../README.en.md), le [guide utilisateur](USER_GUIDE.md) et le [guide de build](BUILD.md) décrivent les fonctionnalités, les paquets et les contrôles d’installation sans déduire leur disponibilité publique d’un build intermédiaire. Les résultats détaillés et leurs limites sont consignés dans [QUALITY.md](QUALITY.md).

## Checkpoint c599a68 — historique de validation

Le checkpoint [c599a68686f4e12acd993fc8778694fbdfff394e](https://github.com/QrCommunication/library-manager/commit/c599a68686f4e12acd993fc8778694fbdfff394e) contient les correctifs de publication/synchronisation Windows NT et de sélection de l’authentification Apple. Les résultats enregistrés pour ce checkpoint sont :

- CI Linux et CodeQL réussis.
- Windows : sidecar C statique compilé ; 305 tests core réussis, neuf échoués et un ignoré. Ce résultat valide une grande partie du correctif NT mais ne constitue pas une CI Windows entièrement réussie.
- macOS Apple Silicon : notarisation complète `Accepted` et empreinte de l’archive ZIP vérifiée. Les attestations de signature, d’agrafage et de Gatekeeper accompagnent le paquet contrôlé. Ces preuves concernent cet artefact ARM64 ; elles ne s’étendent pas à Intel.
- macOS Intel : résultat final encore attendu lors de l’enregistrement de ce checkpoint. Aucune notarisation Intel réussie n’est déduite du résultat ARM64.

Les corrections suivantes sont validées dans leur périmètre source après ce checkpoint et doivent être poussées avant le tag, puis intégrées aux CI du commit final :

- Inventaire GVFS portable et inventaire Windows fondé sur trois requêtes CIM groupées, bornées par un délai global de dix secondes : 22 tests hôte réussis et Clippy core réussi. Les fixtures Windows remplacent les requêtes de données ; elles conservent les jointures et contrôles de sécurité de production.
- Fixtures de chemins du manager : sept tests réussis ; verrou de profil et politique de permissions après acquisition du verrou : trois tests réussis. Une seconde instance refusée ne modifie pas les ACL du profil actif.
- Test du vérificateur de répertoire Windows corrigé sans élargir le contrat des fichiers privés : 12 tests de l’adaptateur compilés pour Windows GNU et Clippy réussi ; revues indépendantes `findings: []`. Cette compilation ne vaut pas exécution Windows native.
- Nettoyage du workflow de livraison : arrêt confirmé avant copie des seuls journaux autorisés, délais bornés et conservation du statut d’échec ; neuf fixtures Docker synthétiques réussies et revue indépendante `findings: []`.

Le workflow de publication contrôlée est poussé et revu à ce checkpoint ; son exécution et la création du tag ne sont pas revendiquées par ces résultats historiques.

## Fonctionnalités et validations source

Checkpoint suivant `4fa22a0` : CI Linux et CodeQL réussies ; les archives macOS Intel et ARM64 ont chacune leurs neuf attestations Apple, une notarisation `Accepted`, une empreinte ZIP et sept empreintes internes vérifiées. Windows passe 317 tests mais les deux sondes PowerShell expirent, y compris celle avec snapshots synthétiques. Les mesures du processus/stdout et l'exécution isolée des sondes doivent en établir la cause avant toute publication. Les archives de ce checkpoint ne doivent pas être mélangées à celles du commit final.

Checkpoint `502eb4f` : les quatre sondes Windows isolées passent sous leur limite inchangée ; les 321 tests du moteur passent ensuite dans la suite complète. Les contrôles restants identifient et font corriger les fixtures Tauri/permissions/imports et la conversion Git CRLF des licences. Le paquet DEB CI réussit neuf contrôles catalogue sur profil synthétique neuf, binaire SHA-256 `2043bc443dcc4b29af8a2da5be3f6cec899ace5bdf4440ef0c9976943fa447e9`. La publication exige toujours la réussite de tous les contrôles sur le commit qui contient ces derniers correctifs.

- Revue persistante et publication atomique : raccords Library/Manager/IPC/UI implémentés. La publication du worker enregistre livre, références de variantes, historique, proposition/revue et tâche terminée dans une transaction commune ; les notifications suivent la libération des verrous.
- Actions groupées : barre commune raccordée aux vues Bibliothèque/Appareils/Assistant, jusqu’à 200 livres, avec disponibilité du fournisseur, confirmation du transfert et retrait réversible du catalogue. Le retrait conserve les fichiers physiques et possède un reçu idempotent.
- Tests ciblés du portage et des contrats : Database 12, secure_fs 8, Storage 12, Transfer 11, Jobs 20, Repository 31, Library 29, Manager 29, Conversion 12, inventaire EPUB 5, Enrichment 23 et Settings 4 réussis. Ces comptes désignent des suites ciblées et ne constituent pas une suite globale additionnable. L’inventaire Devices est couvert par les 22 tests hôte indiqués ci-dessus.
- Interface : 53 tests réussis, contrôle TypeScript/Svelte sans erreur ni avertissement ; revues indépendantes ciblées `findings: []` dans leurs périmètres inspectés.

## Paquets CI exécutés — historique 69641a0

Les quatre parcours suivants ont été exécutés avec le binaire Linux du paquet CI `69641a0`, version 0.2.1, SHA-256 `abc65b87bc671477f9b0418d0ed064b0f232074913d4c4e309c40304e9c341a1` :

| Parcours natif | Résultat |
| --- | --- |
| Actions du catalogue, scénario étendu R5 | 9 contrôles réussis |
| Parcours général | 10 contrôles réussis |
| Assistant | 10 contrôles réussis |
| Transfert CrossPoint synthétique | 4 contrôles réussis |

Le paquet AppImage du même commit `69641a0` a également réussi dix contrôles généraux via son `AppRun` extrait, sur un autre profil synthétique sans réseau. Son lanceur a pour SHA-256 `eb0b254ac0dae6543e6dd7cd02e1baf40a060de95b927313122d6b46323c00aa`. Le payload ELF AppImage a une empreinte distincte de celle du DEB, notamment à cause du traitement de ses chemins de chargement ; le workflow contrôle chaque paquet avec sa propre provenance. Ce passage ne prouve pas l’exécution de l’AppImage avec FUSE.

Le parcours CrossPoint utilise la vraie interface, les IPC publics et un serveur HTTP synthétique sur une adresse privée au port 80, dans un réseau Docker interne sans sortie Internet. Il valide le reçu actif dans Liseuses avant le résultat public de la tâche, sa mise à jour après complétion, sa conservation après navigation aller-retour et l’égalité exacte des octets/SHA du fichier publié avec l’original synthétique. Rapport local : `/tmp/library-manager-0.2.1-qa/crosspoint-receipt-021-r4.json`. Aucun endpoint réservé aux tests ni monkeypatch de l’application n’est utilisé. Ces parcours utilisent des profils synthétiques ; ils ne prouvent ni transfert sur une liseuse physique ni appel fournisseur payant.

Origine des correctifs de plateforme :

- **Lien Windows** : `--enable-tools-static` produit une option Libtool `-static`, qui ne suffit pas pour les dépendances externes. Le build poussé transmet `TOOLS_STATIC=-all-static` et `LIBZ_LDFLAGS=-L/ucrt64/lib -lz`. Le nom de bibliothèque permet à Libtool de conserver sa dépendance zlib et de lier l’archive au bon endroit ; passer directement l’archive dans les flags produisait une archive imbriquée et une référence `uncompress` non résolue. Les trois tests de `build.rs` passent, dont une reproduction réelle de la dépendance transitive. La compilation C du runner Windows `8e37f8a` confirme ce correctif ; l’audit des DLL non système reste strict.
- **Adaptateur Windows** : la publication utilise désormais `NtSetInformationFile(FileRenameInformation)` avec parent épinglé, contrôle d’identité et interdiction de remplacement. La synchronisation ouvre le répertoire lui-même par un nom NT vide relatif au handle, vérifie son identité et demande le vrai flush natif. Aucun assouplissement des ACL ou des protections contre les liens, aucun test ignoré et aucune barrière factice. Revue et compilation statique réussies ; le checkpoint c599a68 ci-dessus consigne les résultats natifs intermédiaires.
- **Notices** : le générateur et les contrôles poussés passent 13 autotests. Le catalogue canonique couvre 701 paquets et 67 limitations documentées. La CI et le workflow de livraison vérifient la concordance des notices embarquées dans chaque paquet final.
- **Déclencheurs** : les pushes documentaires sur `main` déclenchent les builds natifs. Le dernier commit contenant README, documentation et correctifs doit disposer de ses propres artefacts Linux, Windows et macOS avant le tag ; les artefacts du snapshot précédent ne suffisent pas.
- **Notarisation** : retirer du processus Tauri les variables d’authentification Apple vides afin qu’il utilise les identifiants API complets. Le contrôle central HTTP 200 validait l’authentification ; il ne remplaçait pas la soumission `Accepted`, l’agrafage et Gatekeeper. Le checkpoint c599a68 consigne ensuite la preuve ARM64, séparément du résultat Intel.

## Conditions avant publication

Le workflow exige le commit exact du tag sur `main`, les CI Linux et des trois runners natifs réussies, ainsi que CodeQL réussi pour le même commit. Il contrôle les licences et la provenance des paquets, les preuves Apple `Accepted`/agrafage/Gatekeeper pour chaque architecture et le rapport Windows explicitement non signé Authenticode. Il exécute les parcours natifs sur le vrai paquet DEB et l’AppRun extrait, dans des profils synthétiques sans réseau.

La publication suit cet ordre : contrôle des artefacts dans une release brouillon avec manifeste et sommes SHA-256 concordants, publication sans promotion automatique, vérification des téléchargements anonymes et de leurs empreintes, puis promotion comme dernière version. Aucun tag ni publication ne précède la validation du commit final. Une signature macOS seule ne prouve pas la notarisation ; une preuve de checkpoint ou d’une autre architecture ne remplace pas celle du paquet publié.

## Contrats

Revue : enveloppe résultat `{proposal,review}` ; états pending/applied/dismissed/obsolete. La transaction CAS commune écrit livre, fichiers, revue et historique ; une édition personnelle ne consomme pas la proposition. Les propositions legacy restent lisibles et peuvent être traitées explicitement sans révision source fictive.

Sélection : au maximum 200 IDs distincts. Analyse disponible uniquement si fournisseur reconnu/configuré/prêt et modèle renseigné. Le transfert exige une cible connectée accessible en écriture et une validation explicite.

Retrait : `books_remove({requestId,books:[{bookId,expectedRevision}]})`. Retire du catalogue local, conserve originaux/variantes/copies appareil. Journal catalogueRemove réversible et receipt idempotent ; undo vérifie fichiers et collisions.

## Preuves de livraison

Séparer tests source, exécution native Linux, builds Windows/macOS, signature, notarisation Accepted/stapler/Gatekeeper et téléchargements publics. Ne pas attribuer à Windows 11 physique un test exécuté sur runner Windows Server. Ne pas publier de capture contenant la bibliothèque personnelle.

## Historique des checkpoints et validations intermédiaires

Les checkpoints plateforme `6b57b05` et `efd58c7`, puis documentaire `8b81301`, sont des étapes antérieures du travail de livraison. Les résultats ci-dessous portent sur leurs snapshots intermédiaires et ne remplacent pas la validation finale.

Clippy workspace sur le snapshot fonctionnel Linux réussi. À ce stade, les ports avaient les résultats suivants : secure_fs 7, storage 12, transfer 11, manager 26, library 24, conversion 12, inventaire EPUB 5 et devices 19 tests ciblés réussis ; Clippy core toutes cibles réussi. Ces contrôles ne remplacent pas la suite globale finale. Adaptateur Windows cross-compilé et Clippy GNU Windows réussi, sans exécution Windows encore.

Binaire local fonctionnel antérieur au portage : sept contrôles natifs réussis sur profil synthétique réseau isolé (huit vues, disponibilité IA, résolution durable après redémarrage, retrait confirmé, replay idempotent, conservation des octets). Le contrôle undo avait été interrompu par `webdriverUnreachable` ; le passage R3 ci-dessous a ensuite validé le pilotage corrigé. Aucun appel API payant ni écriture appareil.

Natif R3 (profil neuf, network=none) : 8/8 contrôles réussis, dont retrait/replay après redémarrage et undo UI 0→2 avec fichiers/octets préservés. Rapport local `/tmp/library-manager-0-2-1-qa-r3/native-catalogue-report.json`, binaire SHA256 `004a5c9c23cc21040068d0abaf9c339f34096a0af77e380cd073ebac95c34056`. Snapshot fonctionnel antérieur aux ports FS ; il ne remplace pas le contrôle du paquet CI final. Aucun retry transport utilisé au passage réussi.

Suite globale Linux après portage : 295 tests core réussis, un test réseau volontairement ignoré ; workspace build réussi. Ce résultat précède les derniers changements de publication atomique et de cache. Les validations alors en cours sont consignées comme historique, sans conclure au résultat de la publication finale.
