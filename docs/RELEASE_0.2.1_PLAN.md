# Livraison 0.2.1

Demande : validation persistante des propositions, actions communes sur toutes les vues de bibliothèque, installateurs Linux/macOS/Windows et notarisation macOS.

## État courant

Le snapshot fonctionnel, de portage et de documentation validé est poussé dans le commit [69641a07cf2602bb193e06c91b090f1be25e4846](https://github.com/QrCommunication/library-manager/commit/69641a07cf2602bb193e06c91b090f1be25e4846). La version des manifestes est **0.2.1** ; sa livraison reste en préparation. Les correctifs suivants du lien statique Windows, des notices multiplateformes et des déclencheurs CI sont encore locaux : ils devront être poussés et validés sur le commit final avant le tag.

- Revue persistante et publication atomique : raccords Library/Manager/IPC/UI implémentés. La publication du worker enregistre livre, références de variantes, historique, proposition/revue et tâche terminée dans une transaction commune ; les notifications suivent la libération des verrous.
- Actions groupées : barre commune raccordée aux vues Bibliothèque/Appareils/Assistant, jusqu’à 200 livres, avec disponibilité du fournisseur, confirmation du transfert et retrait réversible du catalogue. Le retrait conserve les fichiers physiques et possède un reçu idempotent.
- Portage : façade sécurisée Unix/Windows, stockage et base de données intégrés. Tests ciblés réussis : Database 12, secure_fs 8, Storage 12, Transfer 11, Jobs 20, Repository 31, Library 29, Manager 29, Conversion 12, Devices 19, inventaire EPUB 5, Enrichment 23 et Settings 4. Clippy core toutes cibles réussi sur l’hôte Linux. Ces comptes ne constituent pas une suite globale additionnable.
- Interface : 53 tests réussis, contrôle TypeScript/Svelte sans erreur ni avertissement. Les revues indépendantes ciblées rapportent `findings: []` dans leurs périmètres inspectés.
- Validation globale du snapshot `69641a0` : 315 tests core et huit tests Tauri réussis, avec un test réseau volontairement ignoré ; formatage et Clippy réussis. Les 53 tests frontend et le contrôle TypeScript/Svelte passent. La [CI Linux 38025145928](https://github.com/QrCommunication/library-manager/actions/runs/38025145928) est réussie. La [CI native Windows/macOS 38025145921](https://github.com/QrCommunication/library-manager/actions/runs/38025145921) est en échec : le binaire Windows importe encore `zlib1.dll` et les contrôles des notices rencontrent une divergence entre plateformes. Ces causes ont des correctifs locaux décrits ci-dessous ; leur réussite en CI finale reste attendue.
- Cache des propositions : la baseline native R4 du paquet CI `e351629` reproduisait la disparition du bouton de revue après une édition personnelle, malgré une proposition correctement réancrée en base. Le scénario étendu R5 passe désormais sur le binaire CI `69641a0`, avec les autres parcours natifs consignés ci-dessous. Ces résultats devront être repris sur le binaire du dernier commit de livraison.
- Signature Apple : sept secrets GitHub nécessaires configurés et matériel de signature prêt. Signature, notarisation `Accepted`, tickets agrafés et vérifications Gatekeeper de l’application et du DMG restent à prouver sur les artefacts finaux.
- Publication : release, archives exactes, téléchargements publics et correspondance des sommes SHA-256 restent à effectuer après validation. La documentation utilisateur et les contrats sont synchronisés ; voir [QUALITY.md](QUALITY.md), [USER_GUIDE.md](USER_GUIDE.md) et [BUILD.md](BUILD.md).

## Paquet CI exécuté et correctifs en attente

Les quatre parcours suivants ont été exécutés avec le binaire Linux du paquet CI `69641a0`, version 0.2.1, SHA-256 `abc65b87bc671477f9b0418d0ed064b0f232074913d4c4e309c40304e9c341a1` :

| Parcours natif | Résultat |
| --- | --- |
| Actions du catalogue, scénario étendu R5 | 9 contrôles réussis |
| Parcours général | 10 contrôles réussis |
| Assistant | 10 contrôles réussis |
| Transfert CrossPoint synthétique | 4 contrôles réussis |

Le parcours CrossPoint utilise la vraie interface, les IPC publics et un serveur HTTP synthétique sur une adresse privée au port 80, dans un réseau Docker interne sans sortie Internet. Il valide le reçu actif dans Liseuses avant le résultat public de la tâche, sa mise à jour après complétion, sa conservation après navigation aller-retour et l’égalité exacte des octets/SHA du fichier publié avec l’original synthétique. Rapport local : `/tmp/library-manager-0.2.1-qa/crosspoint-receipt-021-r4.json`. Aucun endpoint réservé aux tests ni monkeypatch de l’application n’est utilisé. Ces parcours utilisent des profils synthétiques ; ils ne prouvent ni transfert sur une liseuse physique ni appel fournisseur payant.

Correctifs locaux à inclure dans le prochain commit :

- **Lien Windows** : `--enable-tools-static` produit une option Libtool `-static`, qui ne suffit pas pour les dépendances externes. Le build transmet maintenant `TOOLS_STATIC=-all-static` et désigne `/ucrt64/lib/libz.a` explicitement. L’audit des DLL non système reste strict. Deux tests de `build.rs` passent, dont une reproduction Linux réelle montrant l’ancien choix de bibliothèque partagée puis le choix de l’archive avec le nouveau lien ; cette preuve ne remplace pas l’exécution Windows native.
- **Notices** : les correctifs de génération et de contrôle entre plateformes passent 13 autotests. La concordance des notices embarquées dans chaque paquet final devra être vérifiée par la CI et le workflow de livraison.
- **Déclencheurs** : les pushes documentaires sur `main` déclenchent désormais les builds natifs. Le dernier commit contenant README, documentation et correctifs doit disposer de ses propres artefacts Linux, Windows et macOS avant le tag ; les artefacts du snapshot précédent ne suffisent pas.

## Conditions avant publication

Le workflow de release est en préparation. Il devra vérifier le commit exact du tag et les CI requises réussies, contrôler les licences et la provenance des paquets, puis exécuter les parcours natifs sur le binaire retenu. Les artefacts seront d’abord contrôlés dans une release brouillon, avec manifeste et sommes SHA-256 concordants. Après publication, les téléchargements anonymes et leurs empreintes devront être vérifiés à nouveau. Aucune réussite finale de signature ou de notarisation n’est déduite de la présence de secrets ou d’un build réussi.

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
