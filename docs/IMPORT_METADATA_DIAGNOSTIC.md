# Diagnostic des imports et de l’analyse des métadonnées

Date : 9 octobre 2026. Version installée examinée : 0.1.1.

## Signalement

Après un import depuis une liseuse, les livres sont marqués présents dans la bibliothèque locale, mais celle-ci affiche zéro livre. Une analyse des métadonnées échoue aussi à 0 % avec le code `invalidInput`.

## Constats

La lecture seule de la base du profil utilisateur relève 134 livres dans `books`, 264 fichiers dans `book_files` et 134 associations dans `device_books`. L’historique comporte 134 tâches d’enrichissement en échec avec `invalidInput`. Le contrôle des chemins réels confirme que les 264 fichiers existent et que leurs tailles et empreintes SHA-256 correspondent à celles enregistrées, y compris pour les 134 originaux. Ils représentent 326 718 406 octets au total. Ces contrôles ont été effectués sans modifier le profil. Ils attestent la présence des entrées et l’intégrité des fichiers ; ils ne prouvent pas leur affichage dans la fenêtre où le problème a été signalé.

Sur une copie de la base, la requête par défaut de `BookRepository` retourne un total de 134 livres et une première page de 100 livres. Un essai du DEB installé 0.1.1, dans un conteneur sans réseau, relève via IPC un total de 134 livres et une page de 48 livres, avec une limite de pagination de 48. L’interface affiche 100 cartes, le compteur « Livres : 134 » et aucune alerte.

Ces premiers essais ont été effectués sans flux continu de rafraîchissements. L’affichage à zéro a ensuite été observé dans la fenêtre réelle et reproduit dans le binaire installé ; le diagnostic complémentaire figure ci-dessous.

## Correction de l’analyse des métadonnées

MiniMax documente l’option `reasoning_split: true` pour séparer le raisonnement du contenu de la réponse dans son [API compatible OpenAI](https://platform.minimax.io/docs/api-reference/text-openai-api). Cette distinction permet au moteur d’analyser la réponse destinée à l’application sans y mêler le raisonnement.

La correction dans `providers.rs` sépare le raisonnement de la réponse. Dans `enrichment.rs`, les erreurs de validation d’une réponse du fournisseur sont classées comme erreurs du fournisseur ; une configuration invalide conserve la classification `InvalidInput`.

## Validation et limites

Les tests ciblés passent : 12 tests d’enrichissement et 11 tests des fournisseurs. Les cinq nouveaux tests de régression ont échoué avant la correction, puis passent avec celle-ci. La validation de la correction des métadonnées compte 175 tests du moteur, 8 tests Tauri et 24 tests de l’interface réussis. Un test nécessitant une API réelle est ignoré. Les résultats de la correction complémentaire de la bibliothèque figurent ci-dessous.

Les contrôles de formatage Rust, Clippy sur toutes les cibles avec avertissements interdits (`-D warnings`), la vérification de l’interface et sa compilation passent. La vérification de l’interface relève zéro erreur et zéro avertissement.

Les modifications source ne sont ni commitées ni publiées. Un premier binaire compilé directement avec Cargo a échoué au contrôle WebDriver : il cherchait l’URL de développement, qui retournait HTTP 404. Cet artefact a été écarté. La compilation par le chemin officiel, `pnpm tauri build --ci --no-bundle -- --locked --offline`, a ensuite réussi. Les 10 contrôles du rapport natif passent avec ce binaire corrigé, sans appel API payant ni écriture sur un appareil.

L’empreinte SHA-256 du binaire corrigé pour les métadonnées est `87aa5c04d6cae95a6424ba74c23ee24a61819ad46c91339b1e5b121937521141`. Une vérification de ce même binaire, sur une copie du profil et sans flux continu de rafraîchissements, relève via IPC un total de 134 livres et une page de 48 livres. L’interface affiche 100 cartes, « Livres : 134 », aucune alerte et aucun chargement en cours.

Le DEB local `0.1.1+metadatafix.20261009`, reconstruit avec ce binaire, se trouve dans `target/metadata-fix-artifacts/`. La version interne de l’application reste 0.1.1 ; le suffixe du paquet permet d’identifier cette construction locale corrigée. L’installation par l’agent était bloquée par la restriction `no_new_privileges`, qui empêche l’élévation via `sudo`. L’utilisateur a ensuite installé ce paquet ; sa présence est maintenant confirmée par `dpkg`. Ce paquet n’a pas été publié et n’inclut pas la correction complémentaire de la bibliothèque décrite ci-dessous.

## Diagnostic complémentaire de la bibliothèque, à 21 h 30

L’utilisateur signale toujours zéro livre après l’installation du paquet corrigé pour les métadonnées. La capture de sa fenêtre montre zéro livre et un chargement actif, avec une recherche vide et aucun filtre sélectionné. L’inventaire de l’appareil est alors en cours à 97,8 %.

Le problème a été reproduit sur une copie du profil avec le binaire installé dont l’empreinte est indiquée ci-dessus : l’émission d’un événement `library:changed` toutes les 50 ms maintient le compteur à zéro, sans carte et avec le chargement actif. Dès que ces événements cessent, le compteur passe à 134, les 100 cartes apparaissent et le chargement se termine.

Dans `LibraryView`, les rafraîchissements continus relancent le délai de 200 ms et invalident la génération des réponses en cours. Tant que l’inventaire émet ces événements, une requête peut donc terminer sans que son résultat soit affiché. Les chargements associés aux filtres multiplient aussi les requêtes.

La correction source ajoute `request-scheduler.ts` et sépare les changements de recherche ou de filtres des rafraîchissements de fond. La dernière recherche est lancée après 200 ms sans changement. Les rafraîchissements de fond sont exécutés en série et regroupés à intervalle de 1 000 ms ; leurs résultats restent affichables pendant l’inventaire. L’arrêt du planificateur lors de la fermeture de la vue empêche les requêtes différées de continuer après celle-ci.

Les huit nouveaux tests du planificateur passent. La suite de l’interface compte désormais 32 tests réussis ; sa vérification relève zéro erreur et zéro avertissement. La revue de non-régression ne relève aucune anomalie (`findings: []`). Les résultats précédents du moteur et de Tauri restent 175 et 8 tests réussis ; ces suites Rust n’ont pas été relancées, leur code étant inchangé depuis la validation précédente.

La compilation native finale a réussi. L’empreinte SHA-256 de son binaire est `1b11a09efaa96bcf258a05fa929025aa56c57ba16927aee56a3d2be874e8af94`. Les 10 contrôles natifs passent. Le scénario de rafraîchissement continu utilise un intervalle nominal de 50 ms pendant cinq secondes, avec 77 événements reçus : à deux secondes puis à cinq secondes, le compteur reste à 134, les 100 cartes sont affichées, le chargement est terminé et aucune alerte n’apparaît.

La fenêtre française réelle de l’utilisateur a ensuite été vérifiée pendant l’inventaire de son appareil : elle affiche « Livres : 134 » et les couvertures. L’application corrigée utilise le profil original, qui contient toujours 134 livres et 264 fichiers ; le processus observé porte le PID 3833464. Elle a été lancée depuis `target/library-fix-artifacts/application/usr/bin/library-manager`, après la fermeture de l’ancienne application. Aucun lanceur permanent n’a été modifié.

Le nouveau paquet local est `target/library-fix-artifacts/library-manager_0.1.1+metadatafix.20261009.2_amd64.deb`. Son empreinte SHA-256 est `9cce2f3ef747d9bf5c5cb61f7f5f1421070c12406d1756051c41f8f4d1ac6ce1`. La version interne de l’application reste 0.1.1. Ce paquet final n’est pas installé dans le système : le paquet précédemment installé par l’utilisateur reste `0.1.1+metadatafix.20261009`. Le binaire extrait du nouveau paquet est celui utilisé pour la vérification dans la fenêtre réelle.

Aucun appel API payant ni appel réel à MiniMax n’avait été effectué dans les phases précédant le signalement de 22 h 06. Une sauvegarde en lecture seule a été prise avant le lancement sur le profil original. L’application peut mettre à jour normalement l’inventaire des appareils dans ce profil ; aucune modification ou suppression de livre, ni écriture sur l’appareil, n’a été effectuée. Les modifications source ne sont pas commitées et les paquets locaux ne sont pas publiés. À cette étape, le parcours MiniMax réel restait à vérifier ; l’affichage des livres pendant l’inventaire était confirmé dans la fenêtre de l’utilisateur. Le diagnostic réel suivant complète ces preuves sans remplacer les résultats historiques.

## Diagnostic complémentaire des métadonnées, à 22 h 06

Une nouvelle analyse d’un livre échoue avec `providerError` après les premières corrections. Un diagnostic sur une copie privée du profil a alors effectué une seule requête réelle de complétion MiniMax. La réponse de 839 octets est du JSON valide et propose uniquement un ISBN. Son contenu brut, les sources et les informations du livre restent privés.

La preuve reprend exactement l’ISBN proposé avec ses tirets. Cependant, `normalize_patch` retire déjà ces tirets et les espaces du numéro proposé avant la comparaison des preuves. La comparaison textuelle rejetait donc deux représentations du même ISBN, en produisant « Evidence value does not match the proposed metadata value » puis l’erreur publique générique `providerError`.

La correction de `matches_evidence_value` normalise désormais la preuve uniquement pour les ISBN textuels, avec le même validateur de structure et de checksum que le patch. Les tirets, les espaces et la casse du X final ne changent plus l’identité comparée. Un autre numéro, un checksum invalide ou une preuve incohérente pour un effacement restent refusés ; les autres champs conservent leur comparaison stricte. Les contrôles des sources réellement consultées, des URLs, des champs autorisés, de la confiance et de la révision restent actifs. La preuve acceptée conserve la forme canonique de l’ISBN.

Le prompt précise également que `evidence.value` doit toujours être une chaîne JSON, y compris pour la représentation compacte d’un tableau, d’un nombre ou de `null`. Cette précision complète le contrat ; la correction du bug ISBN repose sur le code de validation.

La réponse exacte capturée, ses sources et ses avertissements ont été rejoués hors ligne avec la source corrigée : validation réussie, révision attendue 1, champ proposé `isbn` et preuve canonique identique. `auto_applicable=false` conserve la revue manuelle requise. Aucun nouvel appel fournisseur ni application de la proposition n’a été effectué pendant ce rejeu.

Les nouvelles erreurs `providerError` peuvent désormais conserver sept diagnostics statiques autorisés : authentification refusée, requête rejetée, requête échouée, réponse incomplète, réponse malformée, absence de réponse finale et métadonnées invalides. Leur persistance et leur relecture dans les jobs sont contrôlées. L’interface les traduit en français ou en anglais uniquement lorsque le code et la clé correspondent exactement à la liste autorisée ; tout détail inconnu reste masqué. Aucun corps HTTP, réponse brute ni secret n’est affiché. Les détails supprimés des anciens jobs ne peuvent pas être reconstruits à partir de leurs seuls enregistrements.

Validation ciblée : 14 tests d’enrichissement réussis, 14 tests des fournisseurs réussis et un test réseau ignoré, 11 tests des jobs réussis, 41 tests de l’interface réussis. La revue ciblée de non-régression rapporte `findings: []`. Les tests ISBN ont reproduit le rejet avant la correction puis passent avec celle-ci.

La validation finale de cette phase figure ci-dessous. Les preuves natives et les empreintes des paquets précédents décrivent leurs phases respectives.

## Contrôle complémentaire de l’annulation d’un inventaire USB

La première passe globale de cette phase a également révélé une course préexistante dans l’annulation de l’inventaire. La sélection Tokio du Manager pouvait interrompre le futur de dispatch alors que le travail lancé avec `spawn_blocking` continuait. Les données temporaires de l’inventaire pouvaient donc rester visibles avant que le worker ait effectivement terminé son nettoyage.

Le chemin d’indexation USB attend désormais la fin du worker et son nettoyage lors de l’annulation. La modification est limitée à ce chemin ; les connecteurs Calibre et CrossPoint conservent leur traitement existant. Aucun livre ni fichier de l’appareil n’est modifié par ce nettoyage des données temporaires.

Un test utilisant une barrière de synchronisation reproduit la course avant correction puis passe après celle-ci. Le cas existant de progression et d’annulation passe également ; la revue ciblée rapporte `findings: []`. La validation globale et les essais natifs finaux couvrent la source incluant cette correction.

## Validation finale et paquet local `.3`

La suite globale réussit : 183 tests du moteur et un test réseau ignoré, 8 tests Tauri et 41 tests de l’interface. Formatage Rust, Clippy avec avertissements interdits et contrôle Svelte/TypeScript passent. La construction officielle Tauri a réussi en 7 min 42 s. Son binaire porte l’empreinte SHA-256 `f7bd6305603aef7d60ef14840c04bdf397ce2e723dd0227271e63461966ea997`.

Les dix contrôles natifs généraux passent sur ce binaire. Sous un flux continu nominal toutes les 50 ms pendant cinq secondes, 85 événements ont été reçus : le total reste à 134, les 100 cartes DOM restent affichées, le chargement est terminé et aucune alerte n’apparaît. La validation native des diagnostics confirme aussi la liste autorisée transmise via IPC et le détail traduit dans le DOM, avec la même empreinte binaire.

Le paquet final local est `target/metadata-fix-artifacts-v3/library-manager_0.1.1+metadatafix.20261009.3_amd64.deb`, SHA-256 `0e6b82bb7fd8c5af2aad61af7696c356c91e60dc8d2642c8c38563992ea9b2c0`. La version interne de l’application reste 0.1.1. Ce paquet contient les corrections de chargement, de validation ISBN, de diagnostic fournisseur et d’annulation USB.

La fenêtre française réelle utilise maintenant le binaire extrait de ce paquet : processus vérifié PID 4010062, empreinte `f7bd6305603aef7d60ef14840c04bdf397ce2e723dd0227271e63461966ea997`. Elle affiche « Livres : 134 » et les couvertures sur le profil original, qui contient 134 livres et 264 fichiers. Le paquet installé dans le système reste `0.1.1+metadatafix.20261009`, sans suffixe `.3` : l’installation système du nouveau paquet n’a pas été réalisée, l’élévation automatique étant empêchée par `sudo` et `no_new_privileges`. Le lancement local du binaire extrait ne remplace pas durablement le lanceur installé.

Les sources ne sont pas commitées et le paquet local n’est pas publié. Un seul appel réel de complétion MiniMax a été effectué pour le diagnostic de 839 octets ; le rejeu correctif et les essais natifs n’ont pas ajouté d’appel fournisseur. Aucune proposition de métadonnées n’a été appliquée automatiquement et aucune écriture sur la liseuse n’est revendiquée.

## Nouveaux échecs de métadonnées, le 10 octobre à 01 h 00 et 01 h 02

Le paquet `.3` est désormais installé et le binaire examiné conserve l’empreinte `f7bd6305603aef7d60ef14840c04bdf397ce2e723dd0227271e63461966ea997`. Deux nouvelles analyses du même livre, à révision 2, échouent avec le diagnostic `providerDiagnostics.metadataInvalid`. Ces erreurs surviennent après l’installation ; les validations et les états de lancement précédents restent des preuves historiques de leurs phases respectives.

Une seule requête supplémentaire de complétion diagnostique réelle a été effectuée sur une copie privée du profil. La réponse de 1 044 octets porte l’empreinte SHA-256 `2128e38aed700b4a843c392c0a300fdf0c755a75b1944dd6cd973e8dd5d319b0`. Son contenu brut et les informations du livre restent privés. Elle propose uniquement un ISBN, mais la preuve contient une sérialisation JSON supplémentaire de cette valeur textuelle. Un décodage JSON strict unique retrouve exactement la valeur proposée ; la comparaison précédente ne décodait pas cette couche et rejetait donc la preuve.

La nouvelle correction réutilise `normalize_patch` pour les preuves à travers un patch à champ unique. Pour une valeur textuelle, le validateur essaie d’abord le texte littéral, afin de conserver les guillemets appartenant réellement à un titre ; seulement si ce candidat ne correspond pas, il tente un unique décodage JSON strict en chaîne puis la même normalisation. Les encodages imbriqués ne sont pas décodés récursivement. Les tableaux, nombres et `null` encodés en chaîne passent aussi par la validation typée et la normalisation du champ concerné. La preuve persistée conserve sa forme canonique.

Le DTO public continue d’exiger une chaîne pour `evidence.value`. Les garde-fous de types, clés inconnues ou dupliquées, checksum, limites, URLs réellement consultées, confiance, données personnelles, révision et application automatique restent conservés. Le prompt distingue le texte direct, recommandé, d’une unique sérialisation JSON textuelle acceptée, et interdit l’encodage récursif.

Le test reproduisant l’ISBN encodé a échoué avant correction puis passe avec celle-ci. Les 18 tests ciblés d’enrichissement passent, dont les nouvelles régressions pour encodage unique, normalisation des listes/Unicode/langue/nombres, types erronés et encodages imbriqués refusés, ainsi que titres contenant réellement des guillemets.

Le rejeu hors ligne de la réponse exacte de 1 044 octets, empreinte `2128e38aed700b4a843c392c0a300fdf0c755a75b1944dd6cd973e8dd5d319b0`, réussit avec la source corrigée : révision attendue 2, preuve canonique identique et `auto_applicable=false`. Aucun nouvel appel fournisseur n’est nécessaire pour ce rejeu.

## Validation finale et parcours réel du paquet local `.4`

Les validations Rust finales passent : 187 tests du moteur et un test réseau ignoré, 8 tests Tauri, formatage et Clippy. Les 41 tests frontend réussis de la phase précédente restent la référence ; ils n’ont pas été relancés dans cette phase limitée au moteur d’enrichissement. La construction officielle Tauri réussit en 7 min 20 s. Le binaire final porte l’empreinte SHA-256 `a87a43e75aa289dfdaef2ccfe366954b1fa7b181367b2f3cf7eb1881e6336d2d`.

Les dix contrôles natifs généraux passent. Sous un flux continu pendant cinq secondes, 71 événements ont été reçus : total 134, 100 cartes DOM, chargement terminé et zéro alerte. Ces essais concernent le nouveau binaire ; les empreintes des paquets précédents restent historiques.

Un véritable job d’enrichissement a ensuite été exécuté sur le profil original par les API applicatives `book_enrich` puis `run_once`, après sauvegarde et vérification qu’aucun autre job ne serait traité. Aucune mutation SQL directe n’a été utilisée. Le job `dd7b0ba9-e12d-464e-8975-6207d13f103a` a été créé le 9 octobre à 23:18:28 UTC, soit le 10 octobre à 01:18:28 à Paris, et terminé à 23:19:04 UTC, soit 01:19:04 à Paris. Son statut est `completed`, progression 100 %, `proposalStored=true`, `autoApplied=false` et le livre passe à `metadataStatus=needsReview`. L’analyse réelle réussit et la proposition attend une validation manuelle.

Cette phase comprend une complétion diagnostique réelle puis un véritable job fournisseur. Le connecteur conserve ses réessais HTTP prévus ; les preuves comptabilisent la complétion diagnostique et le job exécuté. Le rejeu exact et les essais natifs n’ajoutent pas d’appel fournisseur.

Le DEB final local est `target/metadata-fix-artifacts-v4/library-manager_0.1.1+metadatafix.20261010.4_amd64.deb`, SHA-256 `c23cdcfebcf93a523c789748b360b7fd120b0b9e7f05c1b90ae86e8c58bda3e9`. La version applicative reste 0.1.1.

La fenêtre française réelle utilise le binaire extrait de `.4`, processus PID 183997 et empreinte `a87a43e75aa289dfdaef2ccfe366954b1fa7b181367b2f3cf7eb1881e6336d2d`. Elle affiche les 134 livres et leurs couvertures sur le profil original, qui conserve 134 livres et 264 fichiers. Le paquet installé dans le système reste `.3` ; `.4` est seulement extrait et lancé localement, l’installation automatique étant empêchée par `sudo` et `no_new_privileges`. Les sources ne sont pas commitées et le paquet local n’est pas publié. Aucune proposition de métadonnées n’a été appliquée automatiquement.
