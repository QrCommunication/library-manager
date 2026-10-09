# Enrichissement des métadonnées

Library Manager prépare des propositions bibliographiques : il ne laisse jamais le LLM écrire directement dans un EPUB, remplacer un fichier ou modifier des notes personnelles. L’application applique ensuite une proposition en vérifiant la révision du livre ; une modification concurrente impose une nouvelle vérification.

Chaque recherche utilise le titre, les auteurs et l’ISBN, dans une requête limitée à 512 caractères. Les mêmes outils Internet servent aux six fournisseurs. Au maximum cinq sources sont fournies au modèle, avec leur URL, titre et extrait réellement obtenus. Le contenu d’une page ou d’un livre reste une donnée non fiable ; il ne devient jamais une instruction pour l’application.

Le modèle reçoit les métadonnées bibliographiques et, lorsque l’EPUB permet une inspection sûre, un échantillon de texte de 12 000 caractères maximum. Les notes, évaluations, favoris et informations de lecture ne sont pas inclus. Une clé API du fournisseur est nécessaire ; l’échantillon et les métadonnées sont envoyés à ce fournisseur lors de l’enrichissement. Une lecture EPUB impossible n’empêche pas une recherche fondée sur les métadonnées, mais produit un avertissement.

Une proposition peut modifier le titre, les auteurs, leur classement, la série, son numéro, les genres, les étiquettes, la langue, la description, l’ISBN, l’éditeur et la date de publication. Les indices `0` et `3.5` sont préservés. Les ISBN10/13, les dates, les types, les bornes et les caractères de contrôle sont vérifiés. La validation porte sur les modifications : un ancien ISBN incorrect n’empêche pas de corriger un autre champ.

L’application automatique exige le seuil de confiance configuré et des preuves pour chaque champ modifié. Une preuve doit citer une URL réellement obtenue et une valeur présente dans le titre ou l’extrait correspondant. Le titre, les auteurs, leur classement et la série exigent au moins deux domaines indépendants. Des sous-domaines comme `en.wikipedia.org` et `fr.wikipedia.org` comptent ensemble ; le regroupement est conservateur pour éviter une fausse indépendance. Les citations portent sur les extraits disponibles, sans prétendre avoir lu une page entière.

Une absence de sources, une recherche désactivée ou indisponible, une confiance insuffisante, une langue de titre inconnue ou modifiée, ou un avertissement du modèle imposent une revue manuelle. La langue d’une édition est préservée ; le choix français/anglais de l’interface ne sert pas à traduire ses titres. Une suppression explicite de valeur ne s’applique pas automatiquement faute de preuve positive.

Le JSON du modèle est limité à 64 KiB. Des champs personnels ou inconnus, des clés dupliquées, des valeurs non conformes et des URLs de preuve inventées sont refusés. Les valeurs inchangées sont retirées de la proposition. Les originaux restent intacts ; la normalisation et les variantes optimisées relèvent du service de bibliothèque après validation.

Validation : neuf tests couvrent les propositions partielles, les preuves, les sous-domaines, les champs interdits, les données hostiles, les ISBN, Unicode, les numéros de série, la révision capturée et un EPUB synthétique. Aucune API payante n’est appelée par ces tests.
