# Library Manager — plan de livraison

Session : library-manager-20261009. Source : demande de Rony.

Application Linux autonome : aucun Calibre externe à installer. GPL-3.0, dépôt public QrCommunication/library-manager.

- [x] Autorisation et création des 12 fichiers de cartographie.
- [x] Analyse d’impact et blueprint.
- [ ] Contrats, noyau Rust, SQLite et import sécurisé.
- [ ] EPUB, métadonnées, conversions et optimisation autonomes.
- [ ] Fournisseurs IA, modèles à la volée, outils web, enrichissement et chat.
- [ ] Appareils USB/SD, détection des livres présents et transferts.
- [ ] Connecteur sans fil compatible CrossPoint/Calibre.
- [ ] Identité graphique, bibliothèque, lecteur, filtres, paramètres et langues.
- [ ] Tests, revue sécurité, non-régression et vérification de l’interface.
- [ ] Documentation FR/EN, packaging DEB/RPM et publication vérifiée.

## Règles de validation

Les EPUB privés et secrets restent hors du dépôt public. Une source importée reste intacte. Toute transformation se fait dans la bibliothèque gérée avec sauvegarde. Les intégrations sans identifiants disponibles sont vérifiées par tests de contrat et clairement distinguées de connexions authentifiées réelles.
