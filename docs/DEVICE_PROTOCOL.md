# Appareils et fonctionnement autonome

Library Manager ne requiert aucune installation Calibre. Les livres sont conservés dans une bibliothèque locale privée et envoyés directement à l’appareil.

**État du 10 octobre 2026 : version 0.2.1 en préparation.** Les nouveaux adaptateurs et scénarios automatisés décrits ci-dessous ne constituent pas encore une preuve de transfert physique Windows/macOS. Les preuves historiques Linux/X4 Pro de 0.1.1 restent identifiées séparément ; la construction et les statuts des paquets figurent dans [BUILD.md](BUILD.md) et [QUALITY.md](QUALITY.md).

## USB et carte SD

La détection porte sur les volumes **déjà montés**. Un appareil est identifié indépendamment du nom affiché du point de montage. L’inventaire lit les fichiers et leurs métadonnées sans modifier la carte. La présence affichée dans la bibliothèque correspond aux appareils actuellement connectés ; les anciennes lignes d’inventaire ne suffisent pas à afficher un livre comme présent.

| Plateforme | Découverte dans le code 0.2.1 |
| --- | --- |
| Linux | Lecture de `/proc/self/mountinfo`, volumes sous les racines média autorisées et enfants MTP des montages GVfs accessibles. Ce parcours conserve le fonctionnement existant. |
| Windows | PowerShell système avec un script fixe en lecture seule : `Get-Disk`, `Get-Partition`, `Get-Volume`. Seuls les disques USB ou volumes amovibles possédant une lettre et un système de fichiers sont retenus ; les disques de démarrage ou système sont exclus. |
| macOS | `/usr/sbin/diskutil list -plist external physical`, puis `info -plist` sur les identifiants `diskN`/`diskNsN` strictement validés. Les volumes retenus sont externes et montés sous `/Volumes/` ; leur état de lecture seule doit être fourni explicitement. |

Les commandes natives ne montent, ne formatent et n’écrivent aucun volume. Aucun fragment de commande n’est fourni par l’utilisateur ou le modèle. Les probes Windows/macOS partagent un délai global de **10 secondes**, limitent chaque réponse à **4 MiB** et traitent au plus **128 volumes/identifiants**. Une erreur, un type incorrect ou une identité ambiguë ne donne pas de capacité de transfert.

Les tests Linux et les fixtures JSON/plist vérifient filtrage, identifiants, types et limites. Un test propre à Windows/macOS exécute aussi la véritable commande de son système ; il peut réussir sur un runner sans carte. Ces niveaux ne prouvent ni un branchement physique ni la compatibilité d’un système de fichiers particulier. Une identité ou une garantie native indisponible reste un refus explicite.

Les dossiers système du lecteur, dont `.crosspoint`, sont exclus du classement des livres. Les transferts créent de nouveaux fichiers ; un chemin existant avec un contenu différent ne doit jamais être écrasé implicitement. Avant une écriture, l’identité du volume est revalidée. Les originaux et les variantes de la bibliothèque restent séparés.

Le transfert USB utilise `SecureDir` : répertoire ouvert sans suivre de lien, descendants ouverts relativement aux handles et refus des symlinks/reparse points/junctions. Il crée un fichier temporaire exclusif, copie les octets avec contrôle du SHA-256 et du snapshot de la source, puis vérifie à nouveau la connexion et les identités de la racine et du répertoire cible. `publish_noreplace` publie le fichier atomiquement sans remplacer une destination. Les octets sont synchronisés et la barrière native du répertoire est demandée avant de confirmer la copie ; une garantie non prise en charge produit une erreur. Un doublon n’est reconnu qu’après vérification du contenu. Le nettoyage ne retire que le fichier dont l’identité correspond à celle enregistrée par ce transfert.

Un appareil MTP doit fournir un montage accessible via l’environnement de bureau. Une connexion USB qui n’expose ni stockage de masse ni montage GVfs n’est pas annoncée comme compatible.

### Sélection locale et actions groupées en 0.2.1

La barre commune permet de travailler sur au plus **200 livres locaux** depuis la bibliothèque, ses vues filtrées, Appareils ou l’Assistant : assistant, vérification des métadonnées, transfert et retrait du catalogue. Une ligne présente uniquement sur la carte, sans `bookId` local, doit d’abord être importée ; elle n’est pas un livre local auquel appliquer ces actions. L’analyse nécessite un fournisseur configuré et prêt avec un modèle ; le transfert demande une destination connectée accessible en écriture et une validation explicite du dialogue. Aucun transfert n’est lancé automatiquement à l’ouverture du dialogue.

**Retirer de la bibliothèque** utilise une transaction de catalogue réversible, avec confirmation, révisions et reçu idempotent. Cette action conserve les originaux, les variantes et les copies déjà présentes sur les appareils. L’opération `catalogueRemove` peut être annulée depuis Activité après vérification des fichiers et collisions ; elle ne supprime aucun fichier de la carte. Voir [IPC.md](IPC.md).

### Inventaire et import USB/SD depuis la version 0.1.1

L’inventaire commence par parcourir les dossiers et affiche le nombre d’entrées découvertes. Il lit ensuite les livres avec une barre calculée sur les octets effectivement lus. Le pourcentage reste inférieur à 100 % jusqu’à la fin de l’enregistrement de l’inventaire. Une liaison USB lente peut donc prolonger la lecture ; la barre suit le travail mesuré.

Les livres déjà analysés deviennent visibles pendant l’inventaire. Ceux qui n’existent pas dans la bibliothèque locale sont présentés séparément dans la bibliothèque, avec une indication de leur présence uniquement sur la liseuse. Un livre ainsi détecté reste un fichier de la carte jusqu’à son import. Une déconnexion retire sa présence active ; un inventaire interrompu n’est pas présenté comme terminé.

L’action d’import individuel copie le livre choisi vers la bibliothèque locale. L’action groupée importe tous les livres absents du catalogue, y compris ceux qui dépassent la page affichée. Elle utilise l’inventaire backend complet, borné à **20 000 livres**, indépendamment de la limite de 200 livres locaux de la barre commune. L’aperçu d’un résultat `deviceIndex` reste limité à 500 lignes et 768 KiB ; la pagination UI ne réduit ni l’inventaire enregistré ni le lot sélectionné par `device_import` avec `relativePaths: null`. Les résultats indiquent les livres importés, les doublons et les erreurs. Un fichier refusé n’empêche pas l’import des autres fichiers valides.

L’import conserve la source sur la carte. Avant chaque copie, le backend vérifie l’identité du volume et le fichier inventorié. L’inventaire 0.2.1 ouvre le fichier relativement au répertoire validé et contrôle son identité, taille, dates de modification et de changement avant/après lecture ; le lecteur EPUB borné consomme ce handle sans réouverture libre de chemin. Le contenu copié doit correspondre au SHA-256 inventorié avant insertion locale. Un fichier modifié entre inventaire et copie est refusé ; un contenu déjà local réutilise le livre existant. Ces garanties sont couvertes par tests automatisés.

**Preuve physique historique 0.1.1, sous Linux :** un essai réel sur Xteink X4 Pro en mode carte SD a validé l’inventaire de 134 livres, la progression mesurée, l’affichage des livres absents du catalogue, un import individuel et son réimport sans doublon local ni modification de la source. La carte était montée en lecture seule. Cet essai ne valide pas les nouveaux adaptateurs 0.2.1, un transfert en écriture ou une carte sous Windows/macOS ; les preuves et limites sont détaillées dans [QUALITY.md](QUALITY.md).

## CrossPoint sans fil

CrossPoint expose un serveur HTTP sur le port 80 et un canal WebSocket sur le port 81. Il utilise notamment `/api/status`, `/api/files`, `/upload` et `/mkdir`. Sa découverte UDP répond sur le port 8134. Ces interfaces permettent une connexion directe depuis Library Manager, sans exécuter le plugin ou le programme Calibre.

Le mode File Transfer doit être activé sur la liseuse. Le client appareil est distinct du client Internet utilisé par l’IA : il peut contacter l’adresse LAN choisie pour la liseuse, tandis que les outils web de l’IA refusent les adresses privées.

L’endpoint d’upload du firmware peut écraser un fichier existant. Le connecteur prépare donc un nom temporaire UUID, vérifie le contenu téléchargé, puis appelle `/rename`, qui refuse une destination existante avec une réponse 409. Le chemin final ne doit pas être confié directement à `/upload`. Préserver les chemins des livres déjà lus évite de perdre leur association avec les caches de progression CrossPoint.

Le protocole d’appareil intelligent Calibre et le protocole CrossPoint sont différents. La compatibilité CrossPoint directe ne constitue pas une prise en charge de tout appareil qui utilise un protocole Calibre sans fil.

Sources primaires : [guide CrossPoint](https://github.com/crosspoint-reader/crosspoint-reader/blob/develop/USER_GUIDE.md), [endpoints du serveur](https://github.com/crosspoint-reader/crosspoint-reader/blob/develop/docs/webserver-endpoints.md), [serveur web](https://github.com/crosspoint-reader/crosspoint-reader/blob/develop/docs/webserver.md).

## Protocole Calibre sans fil, intégré à Library Manager

Library Manager peut écouter directement le protocole d’appareil intelligent sur TCP 9090. Dans **Liseuses → Connecter sans fil**, choisir Calibre sans fil, renseigner l’adresse IP locale de l’ordinateur et, si souhaité, un mot de passe. Dans le module Calibre de KOReader, renseigner cette même adresse et ce port puis établir la connexion. Aucun programme Calibre n’est exécuté ou installé. Le serveur reste arrêté tant que cette connexion n’a pas été demandée ; il ne recherche pas les appareils par UDP dans cette version.

Le handshake vérifie les capacités, l’identité de l’appareil et l’authentification du protocole. Ce transport historique n’est pas chiffré : utiliser un réseau local de confiance. Un transfert écrit un nouveau chemin appartenant à Library Manager, puis relit le contenu pour vérifier son SHA-256 avant d’annoncer le livre présent. Les fichiers préexistants ne sont pas remplacés. Un doublon est confirmé par le contenu, pas seulement par son nom.

L’annulation pendant le flux termine le fichier borné en cours, puis demande la suppression du seul chemin créé par ce transfert. Une coupure réseau peut laisser un fichier partiel que le lecteur n’a pas encore indexé : cette situation est signalée et journalisée. Au retour du même appareil, le nettoyage ne vise que les chemins dont Library Manager peut établir la propriété ; les fichiers détectés comme inconnus ou modifiés lors de la vérification sont conservés.

Le nettoyage de reconnexion exige la taille attendue et une relecture avec SHA-256 identique. Les anciens journaux sans taille et les contenus partiels ou différents sont conservés avec avertissement. Le protocole ne propose pas de suppression conditionnelle atomique : la vérification porte sur les octets relus, mais ne peut verrouiller une modification externe entre cette relecture et la commande de suppression.

Les cinq tests de protocole couvrent cadrage, handshake, authentification, inventaire, copie, relecture, dédoublonnage, annulation, nettoyage conservateur et déconnexion avec un client TCP simulé. Ils ne remplacent pas un essai physique sur KOReader. Le CrossPoint étudié utilise son propre serveur HTTP ; sa connexion directe ne dépend pas de ce serveur Calibre.

Sources primaires : [client sans fil KOReader](https://github.com/koreader/koreader/blob/master/plugins/calibre.koplugin/wireless.lua), [protocole d’appareil intelligent Calibre](https://github.com/kovidgoyal/calibre/blob/master/src/calibre/devices/smart_device_app/driver.py).
