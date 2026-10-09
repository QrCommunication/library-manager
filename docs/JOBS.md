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

La réservation vérifie et applique atomiquement le plafond de concurrence, configurable entre un et huit workers. `list()` retourne les 500 dernières tâches. Le JSON métier accepte au plus 256 KiB, les résultats 1 MiB, la profondeur 32 et 512 identifiants distincts de livres. Les champs destinés à contenir des clés, mots de passe ou jetons sont refusés ; les détails internes d’erreur ne sont pas conservés. Les payloads propres à chaque opération sont ensuite validés par le Manager et les services métier. Une tâche stockée malformée est placée en échec avant exécution, sans bloquer la suivante.

Au démarrage, le Manager appelle `recover()` une fois avant de créer des workers. Les anciennes tâches `running` redeviennent `queued`, leur progression recommence à zéro et les résultats intermédiaires nécessaires à la reprise restent disponibles. Une nouvelle réservation incrémente la tentative ; un maximum de 32 tentatives empêche des reprises perpétuelles. Le chat peut ainsi reprendre sa préparation sérialisée et retrouver la réponse existante.

Une attente de configuration ne se réveille qu’après `wake_configuration()`, déclenché par une modification de clé ou de paramètres. Une attente réseau conserve sa date et son budget en base : premier délai de 30 secondes, puis 60 secondes, avec cinq reprises automatiques maximum. Le Manager peut appeler périodiquement `retry_network_due()` ; une date future, une tâche annulée ou un état terminal restent inchangés.

`cancel()` est idempotent et signale les workers actifs via `AtomicBool`. Les états `completed` et `failed` restent terminaux. Les services vérifient le signal aux étapes de travail et avant la publication d’un fichier ; l’annulation est coopérative et ne prétend pas interrompre instantanément une opération système déjà en cours.

`with_event_callback(Arc<dyn Fn(&Job) + Send + Sync>)` permet au Manager d’émettre `job:updated`. Le callback intervient après commit, sans verrou de la file et sans dépendance Tauri dans le moteur. Le polling UI reste possible pour retrouver les tâches après une reconnexion.

`enqueue_with_result()` enregistre le résultat initial dans la même transaction que la création de tâche. Le chat conserve ainsi immédiatement son identifiant de conversation, y compris après un redémarrage précédant la réservation du worker. Les mêmes limites JSON et protections contre les secrets s’appliquent au résultat initial ; un résultat invalide ne crée aucune tâche.

`has_enrichment_for_book()` recherche dans toute la table, sans dépendre de la pagination publique de `list()`. Tous les états comptent, y compris l’annulation et l’échec : l’import/reprise n’engendre pas une boucle de création automatique après un enrichissement déjà planifié. Un enrichissement explicitement demandé par l’utilisateur reste une nouvelle tâche autorisée.

Validation : tests SQLite temporaires de concurrence réelle entre services, plafond, annulation et transitions tardives, redémarrage et tentative périmée, conservation des résultats intermédiaires, attentes explicites, reprise réseau bornée et payloads malformés. Aucun livre ni appareil utilisateur n’est modifié par ces tests.
