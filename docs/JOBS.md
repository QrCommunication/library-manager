# Tâches persistantes

`JobService` utilise la table SQLite `jobs` existante. Les données métier restent dans une enveloppe JSON interne versionnée, contenant le numéro de tentative, le message de progression et le budget de reprise réseau. Le DTO public `Job` conserve son contrat camelCase ; cette enveloppe ne fait pas partie de l’IPC.

```rust
JobService::new(Database) -> Self
enqueue(JobKind, Value) -> Result<Job>
enqueue_with_result(JobKind, Value, Value) -> Result<Job>
has_enrichment_for_book(&str) -> Result<bool>
list() -> Result<Vec<Job>>
get(&str) -> Result<Job>
payload(&str) -> Result<Value>
claim_next(usize) -> Result<Option<ClaimedJob>>
update_progress(&ClaimedJob, f64, &str) -> Result<Job>
update_result(&ClaimedJob, Value) -> Result<Job>
complete(&ClaimedJob, Value) -> Result<Job>
fail(&ClaimedJob, PublicError) -> Result<Job>
wait_for_configuration(&ClaimedJob, PublicError) -> Result<Job>
wait_for_network(&ClaimedJob, PublicError) -> Result<Job>
cancel(&str) -> Result<Job>
recover() -> Result<usize>
wake_configuration() -> Result<usize>
retry_network_due() -> Result<usize>
cancellation_token(&str) -> Result<Arc<AtomicBool>>
```

`ClaimedJob` expose le `Job`, son payload métier, son numéro `attempt` et un signal `cancellation` partagé avec les clones du service. Le Manager transmet ce signal aux services de bibliothèque et de transfert. Chaque mise à jour vérifie le statut `running` et la tentative en base sous transaction immédiate. Un résultat tardif, une ancienne tentative ou une tâche annulée ne peut modifier l’état courant.

La réservation vérifie et applique atomiquement le plafond de concurrence, configurable entre un et huit workers. `list()` retourne les 500 dernières tâches. Le JSON métier accepte au plus 256 KiB, les résultats 1 MiB, la profondeur 32 et 512 identifiants distincts de livres. Les champs destinés à contenir des clés, mots de passe ou jetons sont refusés ; les détails internes d’erreur ne sont pas conservés. Seuls les identifiants de diagnostic fournisseur explicitement autorisés peuvent être persistés avec le code `providerError` ; l’interface les traduit sans afficher une réponse brute ou un secret. Les payloads propres à chaque opération sont ensuite validés par le Manager et les services métier. Une tâche stockée malformée est placée en échec avant exécution, sans bloquer la suivante.

Au démarrage, le Manager appelle `recover()` une fois avant de créer des workers. Les anciennes tâches `running` redeviennent `queued`, leur progression recommence à zéro et les résultats intermédiaires nécessaires à la reprise restent disponibles. Une nouvelle réservation incrémente la tentative ; un maximum de 32 tentatives empêche des reprises perpétuelles. Le chat peut ainsi reprendre sa préparation sérialisée et retrouver la réponse existante.

Une attente de configuration ne se réveille qu’après `wake_configuration()`, déclenché par une modification de clé ou de paramètres. Une attente réseau conserve sa date et son budget en base : premier délai de 30 secondes, puis 60 secondes, avec cinq reprises automatiques maximum. Le Manager peut appeler périodiquement `retry_network_due()` ; une date future, une tâche annulée ou un état terminal restent inchangés.

`cancel()` est idempotent et signale les workers actifs via `AtomicBool`. Les états `completed` et `failed` restent terminaux. Les services vérifient le signal aux étapes de travail et avant la publication d’un fichier ; l’annulation est coopérative et ne prétend pas interrompre instantanément une opération système déjà en cours.

`with_event_callback(Arc<dyn Fn(&Job) + Send + Sync>)` permet au Manager d’émettre `job:updated`. Le callback intervient après commit, sans verrou de la file et sans dépendance Tauri dans le moteur. Le polling UI reste possible pour retrouver les tâches après une reconnexion.

`enqueue_with_result()` enregistre le résultat initial dans la même transaction que la création de tâche. Le chat conserve ainsi immédiatement son identifiant de conversation, y compris après un redémarrage précédant la réservation du worker. Les mêmes limites JSON et protections contre les secrets s’appliquent au résultat initial ; un résultat invalide ne crée aucune tâche.

`has_enrichment_for_book()` recherche dans toute la table, sans dépendre de la pagination publique de `list()`. Tous les états comptent, y compris l’annulation et l’échec : l’import/reprise n’engendre pas une boucle de création automatique après un enrichissement déjà planifié. Un enrichissement explicitement demandé par l’utilisateur reste une nouvelle tâche autorisée.

## Résultat d’analyse et revue persistante

Les nouveaux résultats des tâches `enrich` utilisent l’enveloppe `{ proposal, review }`. La proposition contient les valeurs proposées et leurs preuves ; la revue décrit leur état durable :

| État de revue | Sens |
| --- | --- |
| `pending` | Proposition disponible pour une validation explicite. |
| `applied` | Proposition validée explicitement ou appliquée automatiquement selon la politique d’enrichissement. |
| `dismissed` | Proposition écartée par une autre édition bibliographique. |
| `obsolete` | Proposition dont la révision admissible ne correspond plus, ou analyse d’import qui ne doit pas rouvrir un livre déjà validé. |

`sourceRevision` conserve la révision de référence de l’analyse. `reviewRevision` désigne la révision du livre sur laquelle la proposition en attente peut être examinée ; `resolvedRevision` identifie la révision de résolution. Une édition des notes, favoris, évaluations ou de l’état et de la progression de lecture conserve la proposition `pending` et réancre `reviewRevision`, sans modifier `sourceRevision`. Une édition bibliographique résout les propositions en attente comme `dismissed`, ou `obsolete` lorsque leur révision est périmée. Les anciens résultats contenant directement une `MetadataProposal` restent lisibles sans révision source inventée.

`book_review({ id, jobId, patch, expectedRevision })` exige une tâche `enrich` terminée dont la proposition concerne le livre, ainsi qu’une révision courante compatible avec la revue. Sa transaction écrit ensemble le livre, les éventuelles variantes, l’historique et la résolution `applied`. Un patch vide permet de valider une proposition dont les valeurs sont déjà présentes. Une revue résolue n’offre plus d’action de validation ; la répétition d’une validation déjà appliquée à la même révision restitue le livre sans nouvel historique. Une édition ultérieure exige une nouvelle lecture et ne permet pas de réappliquer l’ancienne proposition. Le contrat complet figure dans [les commandes IPC](IPC.md#validation-durable-dune-proposition).

L’origine de l’analyse reste distincte de son état de revue. `import` respecte l’option d’enrichissement automatique et protège les métadonnées déjà validées. Une demande explicite `manual` peut appliquer une proposition suffisamment prouvée si la révision de référence reste valide, même lorsque l’analyse automatique des imports est désactivée. `assistantReview` produit toujours une proposition à examiner et n’applique jamais automatiquement son patch. Mettre une analyse en file ne signifie pas que ses métadonnées ont été vérifiées. Les conditions de confiance, de sources et d’inspection sont décrites dans [la politique d’enrichissement](ENRICHMENT.md).

## Publication atomique des analyses

La validation explicite par `book_review` conserve la transaction commune décrite ci-dessus. La publication du worker utilise désormais `LibraryService::publish_enrichment` et la méthode interne `JobService::complete_atomically`. Sous le gate de bibliothèque, le verrou du registre des tokens est pris avant l’ouverture d’une transaction SQLite `Immediate`. Avant d’appeler la mutation métier, la file vérifie que la tâche est de type `enrich`, toujours `running`, à la tentative attendue, avec son token courant exact et sans annulation.

La mutation utilise la connexion de cette transaction, sans connexion indépendante ni transaction imbriquée. Les enregistrements du livre, des variantes éventuelles et de l’historique, le résultat `{ proposal, review }` et la transition du job vers `completed` sont enregistrés ensemble. La progression devient `1`, l’erreur et le message d’attente sont effacés, ainsi que la date de reprise réseau. Les contrôles de révision et du périmètre persistant de l’analyse restent applicables ; cette publication ne donne aucun droit supplémentaire au modèle.

Une erreur métier, un résultat dépassant les limites JSON ou refusé par les protections contre les secrets, ou un échec de l’écriture terminale provoque le rollback de la transaction entière. Le signal coopératif d’annulation est recontrôlé après la mutation, avant le commit ; s’il est actif, la mutation et la clôture sont annulées ensemble. Le token de la seule tentative courante est retiré après un commit réussi. Une annulation passant par `cancel()` attend ce verrou et ne peut remettre en cause une tâche déjà terminée.

L’atomicité concerne les enregistrements SQLite. Une variante physique nécessaire à l’application est préparée sous le gate avant cette transaction ; en cas d’échec, le service nettoie la variante non enregistrée. Les originaux restent conservés. Un fichier préparé ne constitue pas à lui seul une publication dans le catalogue.

`complete_atomically` retourne la tâche terminée sans appeler les observateurs. Le caller libère le gate de bibliothèque avant `notify_committed`, qui émet l’événement sans verrou de publication. L’adaptateur Manager doit traiter ce résultat comme déjà commité, sans seconde clôture ni réinterprétation du token terminal comme une annulation de la publication.

À ce stade de préparation de la 0.2.1, les suites ciblées Jobs (20 tests) et Library (29 tests) ont réussi, notamment les refus avant mutation, les rollbacks métier/résultat/écriture finale et les notifications après libération des verrous. Le raccordement final du Manager et sa validation globale restent en cours ; ces preuves ciblées ne constituent pas une preuve du binaire final de CI ou de son parcours natif.

## Reçus de retrait du catalogue

`books_remove` est une commande transactionnelle directe, distincte de la file de tâches. Elle reçoit un `requestId` UUID canonique et une sélection de 1 à 200 couples `bookId`/`expectedRevision`. Toutes les entrées sont validées avant le retrait ; une tâche non terminale concernant un livre bloque son retrait avec `operationConflict`. Le client doit attendre sa fin ou l’annuler, puis relire les livres avant de confirmer une nouvelle demande.

Le retrait crée une opération réversible `catalogueRemove` par livre avec ses instantanés et le reçu de la demande. Il conserve les fichiers originaux, les variantes et les copies présentes sur les appareils. Répéter le même identifiant avec la même sélection canonique, tant que les opérations restent appliquées, restitue les opérations existantes sans nouvelle mutation. Un contenu différent ou une répétition après annulation provoque `operationConflict`. `operation_undo` restaure le catalogue après contrôle des fichiers conservés et des collisions, avec mise à jour transactionnelle de l’historique. Les détails sont décrits dans [le contrat de retrait](IPC.md#retrait-réversible-du-catalogue).

Validation : tests SQLite temporaires de concurrence réelle entre services, plafond, annulation et transitions tardives, redémarrage et tentative périmée, conservation des résultats intermédiaires, attentes explicites, reprise réseau bornée et payloads malformés. Aucun livre ni appareil utilisateur n’est modifié par ces tests.
