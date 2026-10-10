# Politique de métadonnées

Chaque import crée un livre dans le catalogue et conserve son fichier original. L’enrichissement automatique est activé par défaut ; il attend une configuration IA et une connexion utilisables lorsqu’elles manquent.

L’IA reçoit des informations bibliographiques et des sources web. Le contenu du livre, les réponses de moteurs de recherche et les pages consultées constituent des données non fiables, jamais des instructions donnant accès au système.

## Classement et séries

Le classement de fichiers suit Auteur → Série. Les indices peuvent commencer à zéro et être décimaux. Un changement de langue, d’éditeur ou de format ne suffit pas à créer une autre série. La numérotation d’une édition scindée ne doit pas être confondue avec celle d’une édition intégrale : toute correction doit être étayée par des sources identifiant l’édition. Le titre et la série sont conservés lorsque les sources ne justifient pas leur modification.

Les noms sont normalisés en Unicode NFC, compatibles avec FAT et accompagnés d’un identifiant stable pour éviter les collisions. La bibliothèque reconnaît les fichiers par leur SHA-256 ; elle ne dépend pas uniquement de leurs noms.

## Informations proposées

Titre, auteurs, clé de tri auteur, série, indice, genres, tags, langue, description, ISBN, éditeur et date de publication. L’état de lecture, les notes, la note personnelle et les favoris sont des choix de l’utilisateur et ne doivent pas être changés par un enrichissement bibliographique.

Les genres usuels disposent d’identifiants indépendants de la langue et de libellés traduits. Les tags et les genres supplémentaires restent extensibles ; ils ne doivent pas être écrasés par une liste artificiellement fermée.

Une proposition doit contenir une confiance bornée, des preuves par champ et des URLs de sources. Les ISBN sont contrôlés. Les nombres et dates sont validés. La confiance globale seule ne suffit pas à inventer une information absente des sources. Les corrections incertaines restent visibles pour relecture, avec préservation des valeurs existantes.

## Analyse et application

L’analyse groupée exige un fournisseur sélectionné, configuré et prêt, ainsi qu’un modèle renseigné. Sans configuration utilisable, une tâche déjà créée peut attendre la configuration ; cela ne signifie pas que son analyse est terminée.

Une analyse lancée manuellement depuis la fiche ou la sélection peut appliquer une correction automatiquement si les preuves, les conditions de confiance et la révision de référence le permettent. Elle n’est donc pas systématiquement limitée à une proposition en attente. Une analyse issue de l’import ne peut pas écraser les métadonnées déjà vérifiées ni une édition plus récente.

En revanche, les analyses créées par l’outil de vérification de l’assistant portent l’origine `assistantReview` : elles stockent une proposition à examiner et n’appliquent jamais automatiquement son patch. Leur mise en file reste possible sans autoriser une modification bibliographique par l’assistant. Une modification directe par l’assistant exige l’autorisation explicite pour la demande et les livres sélectionnés, ainsi que les contrôles du fichier réellement inspecté et de ses preuves.

## Examiner et valider une proposition

Ouvrir **Examiner les propositions** dans la bibliothèque, **Examiner** auprès d’un livre concerné, ou la fiche du livre. La revue compare les valeurs actuelles aux valeurs proposées et présente les preuves, les sources disponibles et les avertissements. Le badge « à vérifier » peut aussi signaler des métadonnées incomplètes sans proposition disponible.

Les champs identiques ne sont pas affichés comme des changements. Si toutes les valeurs correspondent déjà au catalogue, une validation explicite reste possible : elle enregistre le traitement de la proposition sans inventer de modification bibliographique.

La validation concerne une proposition d’un job d’analyse terminé pour ce livre. Le moteur contrôle les champs et les preuves, la révision actuelle du livre et la révision de revue attendue avant de l’accepter. Le livre, les éventuelles variantes, l’historique et l’état de revue sont enregistrés dans une transaction commune. Une proposition validée passe à l’état `applied` et ne réapparaît plus comme action en attente après un rafraîchissement ou un redémarrage.

Une sauvegarde qui change les informations bibliographiques résout les anciennes propositions en attente : elles deviennent `dismissed`, ou `obsolete` lorsque leur révision ne correspond plus. Modifier uniquement les notes personnelles, les favoris, l’évaluation, l’état de lecture ou la progression conserve les propositions en attente et réancre leur révision de revue sur la nouvelle révision du livre. La révision source de l’analyse reste inchangée ; ces choix personnels ne valident pas la proposition.

Un conflit de révision impose de relire le livre avant une nouvelle action : une ancienne proposition ne doit pas écraser silencieusement une modification concurrente. Les résultats historiques restent lisibles sans leur attribuer une révision source inventée. Répéter une validation déjà appliquée, sans autre changement et à la même révision résolue, ne crée pas un nouvel historique.

## Conservation et annulation

Les réécritures EPUB concernent des variantes gérées, jamais l’original importé. Une révision de livre évite qu’une réponse IA ancienne écrase une modification manuelle récente. Le journal conserve les états utiles à une annulation et la provenance des propositions.

L’annulation d’une revue réversible restaure les métadonnées et les variantes actives depuis l’historique, après les contrôles de révision et d’intégrité. Elle peut remettre la proposition en attente sur une nouvelle révision de revue, sans modifier ses preuves ni inventer une nouvelle analyse. Les originaux et les fichiers nécessaires à l’historique sont conservés.

**Retirer de la bibliothèque** est une action distincte de la revue des métadonnées. Elle retire les livres du catalogue local après confirmation, avec contrôle de révision et reçu de demande idempotent, sans supprimer les originaux, les variantes ni les copies présentes sur les appareils. L’historique permet de restaurer chaque retrait après vérification des fichiers conservés et des collisions avec le catalogue courant. Cette action n’est pas un effacement physique de livres.

Les livres avec des avertissements de structure restent cataloguables. Une transformation peut être refusée lorsque la validité du dérivé ou l’intégrité du texte ne peut pas être garantie.

Les détails des révisions, états de revue et commandes sont décrits dans [IPC.md](IPC.md). Les preuves de validation du code et des paquets sont consignées séparément dans [QUALITY.md](QUALITY.md).
