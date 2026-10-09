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

## Conservation et annulation

Les réécritures EPUB concernent des variantes gérées, jamais l’original importé. Une révision de livre évite qu’une réponse IA ancienne écrase une modification manuelle récente. Le journal conserve les états utiles à une annulation et la provenance des propositions.

Les livres avec des avertissements de structure restent cataloguables. Une transformation peut être refusée lorsque la validité du dérivé ou l’intégrité du texte ne peut pas être garantie.
