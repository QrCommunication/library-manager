# Utiliser Library Manager

Ce guide décrit les parcours de la **version 0.2.1 en préparation**. La disponibilité des paquets et les plateformes effectivement validées seront précisées lors de la publication.

## Configurer les analyses et l’assistant

Dans **Paramètres**, choisissez un fournisseur, renseignez sa clé API et sélectionnez un modèle. Un abonnement au site de chat du fournisseur ne fournit pas automatiquement une clé API. Les appels peuvent être facturés par le fournisseur.

L’analyse des métadonnées exige un fournisseur sélectionné, configuré et prêt, ainsi qu’un modèle renseigné. Si une action est grisée, consultez la raison affichée : clé manquante, modèle absent, sélection vide ou appareil indisponible, selon l’action. Les livres restent consultables lorsque l’IA n’est pas configurée.

Vous pouvez activer ou désactiver l’enrichissement automatique des imports et l’accès au Web. Les clés peuvent rester en mémoire pour la session ; leur conservation utilise le coffre de secrets disponible, sans repli silencieux en texte clair. Les conversations sont conservées localement, mais les demandes IA transmettent au fournisseur les informations et extraits nécessaires. Voir [les fournisseurs](PROVIDERS.md).

## Importer et sélectionner des livres

Importez des fichiers depuis l’ordinateur, ou ouvrez **Liseuses**, puis l’inventaire d’une liseuse connectée pour importer les livres absents du catalogue local. L’import depuis un appareil conserve ses fichiers sur la carte. Suivez ses résultats dans **Activité** : fichiers importés, doublons et éventuels refus.

La recherche, les filtres et le tri permettent de préparer une sélection dans les vues par couvertures, tableau ou groupes, ainsi que dans les vues consacrées aux livres des appareils. Cochez **1 à 200 livres** pour utiliser la barre d’actions commune :

| Action | Parcours |
| --- | --- |
| **Vérifier les métadonnées** | Lance les analyses de la sélection. Les livres ayant déjà une analyse active ne sont pas ajoutés une seconde fois. Consultez ensuite l’activité et les propositions disponibles. |
| **Assistant** | Ouvre une conversation sur les livres cochés. Le bouton de choix des livres permet de revenir à la bibliothèque. |
| **Envoyer à la liseuse** | Dans **Préparer l’envoi**, choisissez un appareil connecté accessible en écriture. Si vous cochez **Optimiser avant l’envoi**, choisissez le profil d’optimisation, puis confirmez l’envoi. La tâche apparaît dans l’activité. |
| **Retirer de la bibliothèque** | Confirmez le retrait du catalogue local. Les fichiers originaux, les variantes et les copies sur les appareils sont conservés. |

Les livres présents uniquement sur un appareil doivent être importés pour disposer d’une fiche locale et des actions qui l’exigent. Un badge de présence sur une liseuse ne signifie pas, à lui seul, qu’un fichier a été importé dans la bibliothèque locale.

## Examiner et valider les métadonnées

Cliquez sur **Examiner les propositions**, ou sur **Examiner** auprès d’un livre ayant une proposition disponible. La fiche présente les valeurs actuelles, les changements proposés, les sources et les avertissements. Un badge « à vérifier » peut aussi signaler des métadonnées incomplètes sans proposition disponible.

Comparez les valeurs et leurs preuves avant de valider. Vous pouvez également valider une proposition dont les valeurs sont déjà présentes : cette validation est enregistrée même lorsqu’aucun champ ne change. Une proposition appliquée disparaît des actions de revue et ne revient pas après un rafraîchissement ou un redémarrage.

Modifier les notes personnelles, les favoris, l’évaluation ou la progression conserve une proposition en attente et l’adapte à la nouvelle révision du livre. Modifier les champs bibliographiques écarte les propositions en attente incompatibles avec cette édition. Si la fiche signale un conflit, rechargez les données et comparez de nouveau les valeurs avant de valider ; une ancienne proposition ne doit pas écraser votre édition récente.

Une analyse lancée explicitement peut appliquer une correction automatiquement si ses preuves, sa confiance et la révision du livre le permettent. Une analyse demandée par l’assistant produit toujours une proposition à examiner. Voir [la politique de métadonnées](METADATA_POLICY.md).

## Demander de l’aide à l’assistant

Les notes personnelles, évaluations et positions de lecture ne sont pas envoyées automatiquement au modèle. Une demande peut utiliser sept outils : rechercher dans le catalogue, inspecter un livre sélectionné, rechercher sur le Web, consulter une page Web, corriger les métadonnées, organiser les fichiers gérés et demander une analyse des métadonnées.

Pour autoriser une correction ou une organisation, cochez **Autoriser l’assistant à modifier et organiser les livres cochés** avant l’envoi. L’autorisation concerne **cette seule demande et les livres sélectionnés**. La case est désactivée après acceptation ; une autre demande exige un nouveau choix explicite. Une demande d’analyse peut créer des tâches sans autoriser la modification automatique des champs bibliographiques.

Par exemple, sélectionnez quelques livres et demandez : « Vérifie leurs titres et auteurs à partir des pages de titre et de copyright. » L’assistant inspecte les fichiers lorsque le format le permet et doit signaler les informations qu’il n’a pas pu vérifier. Pour une correction directe, activez l’autorisation et précisez les changements souhaités.

Le contexte initial contient au plus **32 aperçus de livres sélectionnés**. L’inspection fournit au plus **12 000 caractères** d’extraits réels de pages de titre, de copyright ou d’édition et de chapitre. Une réponse comporte au plus **huit étapes d’outil ou de réponse finale** : une sélection de 200 livres ne prouve donc pas que chaque fichier a été lu ou corrigé. Consultez les résultats et les comptes effectivement annoncés.

Désactiver le Web interdit les outils de recherche et de consultation en ligne. L’assistant ne dispose d’aucun outil de suppression, de transfert, d’exécution de commande ou d’accès à un chemin arbitraire. Les transferts et retraits sont confirmés séparément dans l’interface. Voir [le fonctionnement de l’assistant](CHAT.md).

## Suivre l’activité et annuler une opération

**Activité** présente les tâches en cours, terminées, annulées ou en échec, ainsi que les attentes de configuration ou de réseau. Une analyse mise en file n’est pas encore une analyse terminée. Une attente de configuration exige de corriger les paramètres ou la clé ; une attente réseau peut être reprise automatiquement dans ses limites.

L’annulation d’une tâche arrête les étapes suivantes et laisse les opérations engagées terminer leur nettoyage. Elle conserve les actions déjà terminées. Les analyses créées par une conversation restent des tâches distinctes que vous pouvez annuler individuellement.

Dans l’historique d’**Activité**, utilisez l’action d’annulation d’une opération réversible pour restaurer son état précédent. Le moteur vérifie les révisions, les fichiers conservés et les collisions ; il peut refuser une restauration qui écraserait un état plus récent ou dont les fichiers ne sont plus intacts. Pour un retrait du catalogue, une tâche encore active sur un livre bloque le retrait : attendez sa fin ou annulez-la, puis relisez et confirmez la sélection.

**Retirer de la bibliothèque** conserve les fichiers physiques. L’application ne propose pas, dans ce parcours, de supprimer les originaux, les variantes ou les copies sur une liseuse. Voir [les tâches persistantes](JOBS.md) et [le contrat des opérations](IPC.md).
