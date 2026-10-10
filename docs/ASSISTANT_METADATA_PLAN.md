# Assistant et revue des métadonnées

Session : `assistant-metadata-20261010` — taille large. Livraison **0.2.0 terminée** : code, CI, paquets Linux et téléchargement public validés. La phase initiale .5 reste documentée ci-dessous comme historique. Le profil réel est ouvert avec le binaire du DEB CI ; le paquet système reste .5.

## Utilisation

1. Ouvrir la fiche d’un livre depuis sa couverture ou son bouton dans Assistant. La section **Proposition de l’assistant**, ses preuves et ses avertissements précèdent le formulaire. **Appliquer la proposition** met à jour le catalogue et une variante EPUB normalisée. Une opération réversible permet d’annuler cette application ; les originaux restent intacts.
2. Cocher les livres dans Bibliothèque, puis ouvrir Assistant. **Vérifier les métadonnées des livres cochés** lance les analyses en lot, avec un bilan ajouté/déjà actif/échec. Une analyse terminée prépare une proposition ; elle n’est pas assimilée à une application.
3. Pour demander des modifications, cocher **Autoriser l’assistant à modifier et organiser les livres cochés** puis envoyer la demande. L’autorisation est consommée après l’acceptation du job et doit être donnée à nouveau pour la demande suivante.

## Contrat et garanties

- Le moteur lit réellement les fichiers gérés et vérifie leur taille et leur SHA-256. L’inspection d’édition privilégie les pages titre/copyright, les guides et landmarks EPUB, puis le début du texte narratif. Elle transmet au plus 12 000 caractères, huit pages, 512 Kio par page et 64 Mio par fichier. Il ne s’agit pas d’une lecture intégrale du livre.
- L’inspection distingue l’original immuable de l’EPUB converti ou normalisé. Les deux provenances et l’EPUB actif sont vérifiés sous le verrou de bibliothèque avant une écriture. Une source changée ou une révision périmée provoque un conflit.
- Les outils privés permettent recherche dans la bibliothèque, inspection, recherche et consultation Web, modification bibliographique, organisation et mise en file de vérifications. Aucun shell, SQL, suppression ou chemin hôte arbitraire n’est fourni au modèle.
- Les changements sont limités aux livres sélectionnés et à l’autorisation humaine persistée. Notes personnelles, favoris, évaluation et état de lecture ne peuvent pas être modifiés par l’outil bibliographique. ISBN : checksum valide et preuve dans les métadonnées embarquées, le copyright réellement inspecté ou une source Web réellement récupérée.
- L’organisation renomme et classe les variantes gérées selon auteur/série/titre. Les originaux et anciennes ressources sont conservés pour la traçabilité et l’annulation. Le journal compare les révisions et les snapshots de fichiers avant une restauration.
- Sélection d’action : 200 livres maximum ; contexte initial : 32 aperçus. Le lot de vérification est indépendant de ces 32 aperçus. Le cycle est limité à huit étapes, plus une seule complétion corrective du format pour toute la demande. Chaque réponse est validée strictement avant une action ; la réponse invalide reste éphémère et bornée à 64 Kio. Le planner initial est un appel distinct.
- Les mutations ont des reçus durables : une reprise identique ne réécrit pas le livre et réémet la notification du résultat ; des arguments différents ou une mutation interrompue ambiguë sont refusés. Les extraits et les réponses de lecture ne sont pas conservés dans le journal. L’annulation est vérifiée avant chaque mise en file, et les écritures locales attendent leur nettoyage.
- BookPanel sépare identité, premier chargement et rafraîchissement de fond. Une requête active et une actualisation regroupée suffisent ; les événements ne désactivent ni ne remplacent les champs, et un conflit distant conserve le brouillon jusqu’au rechargement explicite.

## Pourquoi la proposition précédente exige une revue

Le job réel du 10 octobre à 01 h 19 a trouvé un ISBN dans le copyright, sans preuve Web fiable confirmant cette édition. La confiance seule ne suffit pas à l’application automatique. Les règles de preuve, de seuil et de révision restent conservées. Les analyses lancées par l’outil assistantReview restent également manuelles.

## Validation initiale du paquet .5 — historique

- Source finale : 240 tests core réussis, un test réseau volontairement ignoré, huit tests Tauri et 41 tests frontend réussis. Svelte/TypeScript : zéro erreur et avertissement. Format, Clippy avec `-D warnings` et diff conformes.
- Revue : contrôles du périmètre, des empreintes, du journal, de l’annulation et de la réparation du format. Les défauts détectés ont été corrigés : historique terminal bloquant une nouvelle vérification, notification manquante à la reprise et autorisation UI héritée par la demande suivante. Verdict final ciblé : `findings: []`.
- Fournisseur réel : job `6d0c1284-cee0-4d1d-b381-32f4202c1afa` terminé, inspection `bookInspect` journalisée, ISBN du copyright retrouvé. Catalogue, révision et empreintes originales inchangés ; aucune mutation demandée. Le premier échange avait échoué au format sans exécuter d’outil, puis le contrat a été renforcé avec des exemples et une réparation bornée. La configuration Web existante est conservée ; une recherche initiale reste possible, aucun outil Web n’a été utilisé dans cette réponse.
- Paquet final : dix parcours natifs généraux réussis ; proposition réelle visible, appliquée puis annulée sur une copie privée, catalogue et variante normalisée contrôlés, originaux inchangés. Neuf contrôles natifs de l’assistant sont réussis : 33 analyses mises en attente de configuration, relance sans ajout, deux demandes réelles de l’interface enregistrées avec autorisation true puis false pour la même conversation et les mêmes 33 livres. Configuration synthétique sans secret persistant, réseau du conteneur désactivé. Les deux jobs échouent hors réseau, le compositeur se déverrouille et la configuration est restaurée. Le test utilise le bouton réel si une annulation est nécessaire, sans contourner le retour consommé par l’interface ; aucun clic d’annulation n’a été nécessaire dans ce dernier passage. Tempête de 63 événements sur 3 302 ms, sans perte de nœud, focus ou brouillon ni désactivation.
- Profil réel : 134 livres et 264 fichiers vérifiés, 326 718 406 octets avec toutes les empreintes conformes. La fenêtre française du paquet final est ouverte sur cette bibliothèque et ses couvertures. Le binaire actif est extrait du paquet local ; le système conserve encore le paquet `.3`.

## Artefact local .5 — historique

`target/assistant-metadata-artifacts-v5/library-manager_0.1.1+assistantfix.20261010.5_amd64.deb`.

SHA-256 du DEB : `3e200ec21d4dcc746e84f869e06aade6d4cd6c07903e00edb7dc010987307f12`.

SHA-256 du binaire : `4e070208b8d902c47ea199e759769d01cab73982080e9d429da2de24c5ee99a5`.

Les rapports natifs, source, fournisseur et runtime sont conservés dans ce répertoire. Les réponses brutes et les captures bibliographiques restent privées. Aucune écriture matérielle ni application de proposition au profil original n’a été faite par ces validations. Une installation système exige un terminal disposant des droits administrateur ; la session agent `NoNewPrivs` ne les acquiert pas.

## Correction du parcours après installation — 10 octobre, 03:28

Le paquet .5 est effectivement installé et son processus utilise exactement le SHA-256 validé. Huit livres réels portent needsReview, mais une seule analyse enrich terminée contient une proposition. Le statut seul ne prouve pas une proposition disponible ; les nouveaux accès utilisent le résultat terminé et le livre rechargé. Les contrôles précédents validaient des livres déjà cochés. Le parcours initial masquait le panneau d’action et LibraryView n’avait aucun bouton de lot. La version 0.2.0 corrige ce parcours : actions visibles depuis Bibliothèque, panneau permanent à zéro sélection, accès direct à la fenêtre de revue et disparition des propositions déjà appliquées. Les preuves du paquet .5 ci-dessus restent historiques ; la livraison finale est consignée ci-dessous.

## Publication 0.2.0 autorisée — 03:38

Versions synchronisées dans les quatre manifestes/lockfile, dépendances inchangées. Installer DEB/RPM/AppImage construits par la CI sur le SHA poussé, contrôlés puis publiés sur le même tag. Les tests natifs renforcés partent de zéro sélection et ouvrent une proposition persistée depuis la bibliothèque. Documentation, tests et cartographie accompagnent le commit ; aucune publication n’est déduite d’une compilation locale.

## Livraison 0.2.0 terminée

- Code publié et tag `v0.2.0` : `fbcd2f7c5828be6e83a9982a66bc85fa0bf7bbcc`. [CI Linux 38014777810](https://github.com/QrCommunication/library-manager/actions/runs/38014777810) et [CodeQL 38014777024](https://github.com/QrCommunication/library-manager/actions/runs/38014777024) réussis sur ce code. Les sources de l’archive CI ont été comparées aux 59 fichiers suivis attendus.
- Validation automatisée : 240 tests core réussis et un test réseau ignoré, huit tests Tauri et 41 tests frontend réussis ; format, Clippy et Svelte/TypeScript conformes, zéro erreur et avertissement. La revue ciblée finale retourne `findings: []`.
- Paquets exacts CI conservés dans `target/release-artifacts-v0.2.0/` : DEB amd64, RPM x86_64 et AppImage. Ressources du moteur MOBI et licences vérifiées. Les 24 contrôles natifs réussis couvrent dix scénarios assistant, quatre scénarios de revue et dix scénarios généraux. La revue applique puis annule une proposition sur une copie privée ; elle ne modifie pas les métadonnées du profil original.
- DEB : `library-manager_0.2.0_amd64.deb`, SHA-256 `fa241b3b127bcbdaffbeb6ebf18184aebc9d0fecd04a04798d32ef3c19525586`. Binaire extrait utilisé pour les contrôles et le profil réel : `b331925c2cb1a41a54c4b73d2ba7c12dbefb3d42731a36887c0d90c27b173049`.
- [Release publique v0.2.0](https://github.com/QrCommunication/library-manager/releases/tag/v0.2.0) publiée avec les trois installateurs, les sources, les licences, les rapports natifs, le résumé de validation et `SHA256SUMS`. Les onze premiers assets ont été téléchargés sans authentification : HTTP 200 et empreintes identiques aux artifacts publiés. Le rapport `public-verification.json`, douzième asset, a également été retéléchargé anonymement avec une empreinte identique : `4a531d87d41c9cbc578d8fe05a44be17f445fb9ac4db4e075681c4e4f2a1ea23`.
- Profil réel rouvert avec le binaire CI, PID `720055`. La capture privée montre la version **0.2.0**, **134 livres**, les couvertures et l’entrée **Examiner les propositions**. Le système conserve le paquet .5 : le lancement extrait ne constitue pas une installation administrateur de 0.2.0.

Les contrôles natifs CI n’ont réalisé aucun nouvel appel API payant ni écriture sur un appareil physique. L’extraction de l’AppImage et la présence de ses ressources MOBI et licences sont vérifiées ; le lancement AppRun/FUSE et l’interface graphique RPM ne sont pas attestés. Une capture générale WebKit facultative a expiré, mais le parcours général complet a réussi sans capture sur un profil neuf. Les réponses et captures bibliographiques du profil utilisateur restent privées.
