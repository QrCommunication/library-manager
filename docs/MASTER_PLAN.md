# Library Manager — plan de livraison

Session : library-manager-20261009. Source : demande de Rony.

Application Linux autonome : aucun Calibre externe à installer. GPL-3.0, dépôt public QrCommunication/library-manager.

- [x] Autorisation et création des 12 fichiers de cartographie.
- [x] Analyse d’impact et blueprint.
- [x] Contrats, noyau Rust, SQLite et import sécurisé.
- [x] EPUB, inspection, conversions et optimisation autonomes.
- [x] Fournisseurs IA, modèles à la volée, outils web, enrichissement et chat.
- [x] Appareils USB/SD, détection des livres présents et transferts.
- [x] Connecteurs CrossPoint HTTP et protocole Calibre sans fil autonome.
- [x] Identité graphique, bibliothèque, lecteur, filtres, paramètres et langues.
- [ ] Tests, revue sécurité, non-régression et vérification de l’interface.
- [ ] Documentation FR/EN, packaging DEB/RPM et publication vérifiée.

## Avancement vérifié

- 33 commandes IPC, cinq événements et 356 traductions FR/EN ; contrôle TypeScript/Svelte sans erreur ni avertissement, 22 tests frontend réussis.
- Noyau SQLite/recherche/EPUB/optimisation/stockage/appareils/Web/conversion/fournisseurs/lecteur/enrichissement/import/chat/paramètres/Manager : 157 tests intégrés réussis sous Rust 1.99.0, Clippy all-targets sans avertissement ; un smoke réseau explicitement ignoré par défaut.
- Collection réelle : 133 EPUB catalogués en lecture seule, 133 aperçus textuels, 130 validations de structure et 3 avertissements explicites.
- Import réel dans un profil temporaire privé : 133 livres et originaux, 130 variantes normalisées, 133 couvertures, zéro erreur ; 133 réimports dédoublonnés avec IDs identiques, SHA-256 des 133 sources inchangés et concordants avec les originaux. Sept livres restent à revoir (huit avertissements), sans appel IA ni écriture sur carte.
- Moteur MOBI embarqué : libmobi 0.12 compilé, testé et sans dépendance Calibre ; MOBI6 natif validé par un second parseur, dont texte multilingue et image.
- Web commun : recherche anonyme réelle vérifiée, URLs HTTPS publiques, DNS épinglé, taille/durée/redirections bornées.
- Catalogue Z.ai : 21 modèles récupérés réellement ; autres fournisseurs vérifiés par contrats et serveurs locaux de test, aucun appel payant.
- Lecteur EPUB : sommaire imbriqué, sections assainies, progression persistante et tests de contenu actif malveillant réussis.
- Vérification IAB du lecteur : titre et paragraphes de démonstration visibles dans srcdoc avec sandbox strict ; fermeture du lecteur conserve les 12 livres présents filtrés sur Xteink et le mode Tableau.
- Identité graphique, tokens CSS, shell et vues bibliothèque, fiche du livre, appareils, chat, paramètres, activité et lecteur intégrés ; parcours de démonstration vérifiés dans le navigateur.
- Calibre sans fil : cinq tests de protocole réussis, dont conservation d’un fichier modifié après interruption et vérification de taille/SHA-256 avant nettoyage.
- Revue de sécurité et seconde vérification : le défaut de nettoyage identifié est corrigé et couvert ; limites du protocole et de la revue documentées.
- Scripts de notices et de smoke natif : cinq tests purs réussis chacun ; 648 dépendances inventoriées, notices Linux runtime manquantes intégrées et 43 limites d’inventaire de sources explicitement indiquées.
- Chat persistant : neuf tests ciblés réussis, recherche de bibliothèque validée et contexte borné ; aucune exécution de commandes ou de SQL produit par le modèle.
- Bibliothèque : huit tests de parcours réussis, variantes et journal transactionnels, corrections avec contrôle de révision, annulation avant publication et conservation de la progression de lecture.
- Environnement de packaging Ubuntu 22.04 isolé, Node 26.11.1, pnpm 10.33.0, Rust 1.99.0 ; aucune installation Calibre dans cet environnement.
- Dépôt public initialisé, licence GPL-3.0 reconnue par GitHub.

Les éléments ci-dessus ne constituent pas encore une validation de l'application native complète.

## Règles de validation

Les EPUB privés et secrets restent hors du dépôt public. Une source importée reste intacte. Toute transformation se fait dans la bibliothèque gérée avec sauvegarde. Les intégrations sans identifiants disponibles sont vérifiées par tests de contrat et clairement distinguées de connexions authentifiées réelles.
