# Assistant, recherche et actions sur les livres

L’assistant utilise le fournisseur et le modèle sélectionnés dans les paramètres. Une clé API du fournisseur est nécessaire ; un abonnement à son site web ne fournit pas automatiquement une clé API. Les conversations et les sources associées aux réponses sont conservées localement dans SQLite. Les requêtes au fournisseur transmettent les informations utiles à la demande : ce service n’est pas un mode hors ligne.

## Choisir les livres et autoriser une demande

Cocher les livres dans la bibliothèque, puis ouvrir **Assistant** pour travailler sur cette sélection, jusqu’à **200 livres**. Sans sélection, l’assistant peut rechercher dans la bibliothèque ; le bouton de choix des livres permet de revenir à celle-ci. Les fiches de la sélection donnent accès au livre et à ses propositions disponibles.

La case **Autoriser l’assistant à modifier et organiser les livres cochés** est désactivée par défaut. L’activer autorise les outils de modification et d’organisation pour **la demande envoyée et ses seuls livres sélectionnés**. Après acceptation de la demande, la case est désactivée : une demande suivante exige un nouveau choix explicite. Cette permission ne donne pas accès à un chemin arbitraire ni aux autres livres de la bibliothèque.

Le backend enregistre la question, les identifiants sélectionnés et cette autorisation avant le démarrage du traitement. Une reprise conserve exactement ce périmètre. Le modèle, les pages web et le texte des livres ne peuvent pas changer ces droits.

## Outils disponibles

Les outils internes suivent un protocole JSON strict, validé avant exécution. Ils ne sont pas des commandes shell ou de nouvelles commandes IPC publiques.

| Outil | Résultat et conditions |
| --- | --- |
| `librarySearch` | Recherche dans le catalogue avec filtres et tri autorisés, au plus 48 résultats par page, sans SQL fourni par le modèle. |
| `bookInspect` | Inspection du fichier réel d’un livre sélectionné, avec contrôle de taille et SHA-256 et extraits bornés lorsque le format le permet. |
| `webSearch`, `webFetch` | Recherche ou lecture de sources publiques réelles lorsque le Web est autorisé. |
| `updateMetadata` | Correction des champs bibliographiques d’un livre sélectionné, après inspection, autorisation de modification et contrôles de preuve, d’intégrité et de révision. |
| `organizeBooks` | Organisation selon la convention gérée auteur/série/titre, avec autorisation, contrôle des révisions et conservation des originaux. Aucun chemin de destination libre. |
| `verifyMetadata` | Mise en file d’analyses des livres sélectionnés, sans autoriser une modification bibliographique. Ces analyses produisent des propositions à examiner et ne les appliquent jamais automatiquement. |

L’inspection peut fournir au plus **12 000 caractères** de pages de titre, de copyright ou d’édition et de chapitre. Elle distingue l’original de l’EPUB réellement lu, qui peut être une variante convertie. Les extraits restent temporaires et sont relus lors d’une reprise ; ils ne prouvent pas une lecture intégrale du livre. Un format non inspectable ou une variante dérivée est signalé, sans inventer de contenu.

Avant une correction, le moteur recontrôle la révision, l’original et les fichiers impliqués. Un ISBN doit avoir un checksum valide et être étayé par les métadonnées embarquées, le texte réellement inspecté ou une source effectivement récupérée pendant la demande. Une URL inventée ou un extrait absent ne constitue pas une preuve. Les outils de correction ne modifient pas les notes personnelles, les favoris, l’évaluation ni l’état de lecture.

L’assistant n’a aucun outil de suppression, transfert, SQL, exécution de commande ou lecture de chemin arbitraire. Les opérations de transfert et de retrait du catalogue sont des actions explicites de l’interface, distinctes de son autorisation de modification.

## Recherche et contexte bornés

Un appel préparatoire au modèle propose un plan de recherche en lecture seule : texte, auteurs, séries, genres, étiquettes, langues, formats, états de lecture et de métadonnées, tailles, favoris et tri. Les filtres doivent correspondre aux facettes de la bibliothèque et les champs de tri sont autorisés côté moteur. Le plan est exécuté par des requêtes préparées ; un plan incorrect ou indisponible entraîne une sélection prudente triée par titre, avec un avertissement indiquant qu’elle ne couvre pas toute la bibliothèque.

Le planner ne possède pas l’état de connexion des appareils et refuse les filtres de présence sur une liseuse. Les filtres d’appareil de l’interface utilisent les appareils réellement connectés du service de bibliothèque.

Le contexte initial contient la question, les métadonnées bibliographiques d’au plus **32 livres sélectionnés**, les résultats réels de recherche et les **20 derniers messages antérieurs** de la conversation. Les identifiants des autres livres sélectionnés restent disponibles aux outils bornés. Une sélection de 200 livres ne signifie donc ni l’envoi de 200 fiches complètes ni l’inspection de chaque fichier.

Les notes personnelles, évaluations et positions de lecture ne sont pas envoyées automatiquement. Le contexte JSON est limité à **384 KiB** : les aperçus trop volumineux sont raccourcis avec un avertissement indiquant que le texte omis n’a pas été examiné. La question courante et les identifiants sélectionnés sont préservés ; les nombres de résultats restent cohérents avec ce qui a été transmis.

Une réponse comporte au plus **huit étapes d’outil ou de réponse finale**. Une seule tentative supplémentaire peut corriger une réponse qui ne respecte pas le protocole, soit neuf appels maximum dans ce cycle, hors planner préalable. Le texte invalide est temporaire, borné et traité comme une donnée non fiable ; il ne peut pas autoriser une action. Ces limites empêchent de promettre une correction de toute la sélection sans résultats qui l’établissent.

## Sources web

Lorsque le Web est activé, les mêmes outils servent à tous les fournisseurs. La préparation peut consulter jusqu’à deux URLs présentes dans la question et compléter le contexte par une recherche fournissant au plus cinq sources. Les URLs sont retirées de la requête de recherche générale ; les étapes d’outils suivantes peuvent récupérer d’autres sources dans leurs limites.

Le client Web refuse notamment les services locaux, adresses privées et redirections dangereuses. Une citation ou preuve doit correspondre à une page effectivement récupérée. Les extraits ne prouvent pas la lecture d’un site entier. Désactiver le Web interdit ces outils ; l’assistant doit signaler une absence de vérification en ligne lorsqu’elle compte pour sa réponse.

Les métadonnées, fichiers, résultats de recherche et pages consultées restent des données non fiables. Les instructions qu’ils contiennent ne peuvent accorder de permission, changer la sélection, déclencher une commande ou révéler des secrets.

## Historique, reprise et annulation

La préparation persiste le message utilisateur et un identifiant de conversation utilisable par les tâches. Répéter une préparation sérialisée réutilise la même demande ; préparer volontairement une nouvelle question crée un nouveau message. Les réponses tardives restent associées à leur question d’origine. Les messages futurs et les autres conversations sont exclus du contexte. Les listes sont limitées aux 200 conversations actives les plus récentes et aux 200 derniers messages d’une conversation.

Les mutations des outils conservent des reçus durables. Une reprise du même identifiant d’outil avec les mêmes arguments réutilise son résultat enregistré ; une action interrompue au résultat incertain n’est pas réexécutée automatiquement. Les résultats et erreurs exécutés servent à la réponse : une mise en file d’analyse ne vaut pas une analyse terminée, et un échec ne peut pas être annoncé comme une correction réussie.

L’activité montre les étapes réellement exécutées. L’annulation arrête les étapes suivantes et attend le règlement des mutations engagées avant de libérer le worker et le profil. Les actions déjà terminées et les analyses déjà créées restent visibles dans l’activité ; les opérations réversibles peuvent être annulées depuis l’historique. Les fichiers originaux sont conservés.

## Actions groupées de l’interface en 0.2.1

La barre commune des vues de bibliothèque propose **Assistant**, **Vérifier les métadonnées**, **Transférer** et **Retirer de la bibliothèque**, jusqu’à 200 livres. L’analyse nécessite un fournisseur sélectionné, configuré et prêt, ainsi qu’un modèle renseigné ; les analyses déjà actives sont dédupliquées.

Le transfert demande un appareil connecté accessible en écriture, un profil et une confirmation. Le retrait du catalogue demande une confirmation et vérifie les révisions ; il conserve les originaux, variantes et copies sur les appareils. Son reçu idempotent évite les doublons en cas de répétition de la demande, et son opération d’historique permet une restauration avec contrôle des fichiers et collisions. Ces actions d’interface ne sont pas de nouveaux outils de suppression ou de transfert accordés au modèle.

Les propositions à examiner s’ouvrent dans la fiche et leur validation est persistante, même lorsque les valeurs sont déjà identiques. La politique distingue une analyse manuelle pouvant appliquer une correction sous ses contrôles de preuve et de révision, des analyses `assistantReview`, toujours soumises à revue. Voir [METADATA_POLICY.md](METADATA_POLICY.md) et [IPC.md](IPC.md).

Les tests utilisent des bases temporaires, des livres synthétiques, des réponses contrôlées et des validations pures pour vérifier droits, preuves, révisions, reprise, limites et protection des données personnelles. Ces tests n’appellent aucune API payante et ne constituent pas une preuve de GUI sur une plateforme particulière. Les preuves du paquet distribué sont consignées dans [QUALITY.md](QUALITY.md).
