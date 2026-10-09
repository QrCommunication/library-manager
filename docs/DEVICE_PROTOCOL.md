# Appareils et fonctionnement autonome

Library Manager ne requiert aucune installation Calibre. Les livres sont conservés dans une bibliothèque locale privée et envoyés directement à l’appareil.

## USB et carte SD

La détection utilise les volumes montés Linux et les montages GVfs déjà accessibles. Un appareil est identifié indépendamment du nom affiché du point de montage. L’inventaire lit les fichiers et leurs métadonnées sans modifier la carte. La présence affichée dans la bibliothèque correspond aux appareils actuellement connectés ; les anciennes lignes d’inventaire ne suffisent pas à afficher un livre comme présent.

Les dossiers système du lecteur, dont `.crosspoint`, sont exclus du classement des livres. Les transferts créent de nouveaux fichiers ; un chemin existant avec un contenu différent ne doit jamais être écrasé implicitement. Avant une écriture, l’identité du volume est revalidée. Les originaux et les variantes de la bibliothèque restent séparés.

Un appareil MTP doit fournir un montage accessible via l’environnement de bureau. Une connexion USB qui n’expose ni stockage de masse ni montage GVfs n’est pas annoncée comme compatible.

### Inventaire et import USB/SD depuis la version 0.1.1

L’inventaire commence par parcourir les dossiers et affiche le nombre d’entrées découvertes. Il lit ensuite les livres avec une barre calculée sur les octets effectivement lus. Le pourcentage reste inférieur à 100 % jusqu’à la fin de l’enregistrement de l’inventaire. Une liaison USB lente peut donc prolonger la lecture ; la barre suit le travail mesuré.

Les livres déjà analysés deviennent visibles pendant l’inventaire. Ceux qui n’existent pas dans la bibliothèque locale sont présentés séparément dans la bibliothèque, avec une indication de leur présence uniquement sur la liseuse. Un livre ainsi détecté reste un fichier de la carte jusqu’à son import. Une déconnexion retire sa présence active ; un inventaire interrompu n’est pas présenté comme terminé.

L’action d’import individuel copie le livre choisi vers la bibliothèque locale. L’action groupée importe tous les livres absents du catalogue, y compris ceux qui dépassent la page affichée. Les résultats indiquent les livres importés, les doublons et les erreurs. Un fichier refusé n’empêche pas l’import des autres fichiers valides.

L’import conserve la source sur la carte. Avant chaque copie, le backend vérifie l’identité du volume et le fichier inventorié. Il compare l’empreinte de la copie au SHA-256 relevé pendant l’inventaire avant d’ajouter le livre au catalogue. Un fichier modifié entre inventaire et copie est refusé ; un contenu déjà local réutilise le livre existant. Ces garanties sont couvertes par tests automatisés. Un essai réel sur Xteink X4 Pro en mode carte SD a validé l’inventaire de 134 livres, la progression mesurée, l’affichage des livres absents du catalogue, un import individuel et son réimport sans doublon local ni modification de la source. La carte était montée en lecture seule ; les preuves et limites sont détaillées dans [QUALITY.md](QUALITY.md).

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
