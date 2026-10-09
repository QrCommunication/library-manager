# Inventaire de liseuse et import vers la bibliothèque

Demande du 9 octobre 2026 : sur une Xteink X4 Pro connectée en mode carte SD, l’application détecte le volume mais l’inventaire paraît bloqué. Afficher sa progression réelle, présenter les livres de la liseuse absents du catalogue local et permettre leur import individuel ou groupé.

Ce document suit cette correction. Les cases cochées indiquent uniquement des étapes vérifiées ; les résultats de la version précédente dans `MASTER_PLAN.md` ne valident pas ces nouveaux parcours.

## Contexte vérifié

Le projet utilise Tauri 2, un moteur Rust autonome, SQLite, Svelte 5 et des traductions FR/EN. Le moteur inventorie les montages accessibles ; les tâches transmettent leur état au shell par `job:updated`. Les livres de la liseuse et les livres locaux ont des identités distinctes : une présence sur carte ne crée pas automatiquement un livre local.

La cartographie complète existe dans `/home/rony/.Codex/projects/library-manager/memory/` : les douze fichiers attendus sont présents. Le signal d’absence vise un autre emplacement, sous `.claude`. Conserver les documents existants et les mettre à jour avec les changements de cette tâche. Le fichier `AGENTS.md` est absent à la racine du dépôt ; les instructions globales fournies dans la conversation s’appliquent.

## Suivi

- [x] Vérifier l’existence des douze documents de cartographie et préserver leur contenu.
- [x] Consigner la demande, le contexte technique et les critères de validation.
- [x] Identifier le travail coûteux de l’inventaire et le point où sa progression cesse d’être visible.
- [x] Définir les unités mesurables de progression, les étapes et la publication des résultats partiels.
- [x] Adapter le moteur, les contrats IPC et l’affichage de progression.
- [x] Afficher dans la bibliothèque les livres détectés sur l’appareil et absents du catalogue local, avec un indicateur explicite.
- [x] Ajouter l’import d’un livre de l’appareil et l’import groupé des livres absents du catalogue local.
- [x] Mettre à jour les libellés FR/EN et les documents de cartographie concernés.
- [ ] Exécuter les tests ciblés et la validation de non-régression.
- [ ] Vérifier le parcours rendu et distinguer les essais simulés des essais sur la liseuse physique.

## Critères de validation

| Parcours | Résultat attendu |
| --- | --- |
| Inventaire lent | L’interface affiche l’étape en cours et les unités déjà traitées. Le pourcentage correspond au travail mesuré ; une durée arbitraire ne fait pas avancer la barre. |
| Découverte en cours | Les résultats déjà disponibles deviennent visibles selon le mécanisme de publication retenu. Une interruption ne présente pas l’inventaire comme complet. |
| Livre déjà local | Le rapprochement conserve l’identité du livre local et sa présence sur l’appareil. |
| Livre absent du catalogue | La bibliothèque montre une entrée identifiable comme présente uniquement sur la liseuse, sans lui attribuer les actions qui exigent un fichier local. |
| Import individuel | Une action explicite copie le livre vers la bibliothèque gérée, conserve la source sur la carte et applique le dédoublonnage existant. |
| Import groupé | L’action cible les livres absents du catalogue. Elle restitue les réussites et les erreurs sans annoncer un succès global si un fichier échoue. |
| Déconnexion ou annulation | La tâche indique son état réel, retire la présence active après déconnexion et n’importe pas de fichier depuis une identité de volume devenue invalide. |
| Nouvel import du même contenu | Le catalogue réutilise le livre existant ; aucune entrée locale en double. |
| Français et anglais | Les libellés de présence, progression, import et erreur sont disponibles dans les deux langues. |

L’inventaire est une lecture de la carte. L’import utilise les protections du stockage local et revalide l’appareil avant lecture. Une liste tronquée destinée à l’interface ne doit pas devenir la source exhaustive de l’import groupé.

## Documentation à synchroniser

| Domaine modifié | Cartographie à actualiser |
| --- | --- |
| Inventaire, import et orchestration Rust | `backend_services.md` |
| Contrats de progression ou de livre détecté | `backend_models.md` |
| Commandes IPC | `backend_controllers.md`, `api_routes.md` |
| Schéma ou requêtes d’inventaire | `database_schema.md` |
| Bibliothèque, appareils et traductions | `frontend_spa.md` |
| Flux entre moteur et interface | `architecture_overview.md` |
| Cause de lenteur corrigée et limites restantes | `tech_debt.md` |

## Preuves de cette correction

Validation ciblée : 14 tests devices, 14 tests EPUB, 13 tests Manager (dont lot de 202 livres et refus du fichier modifié), un test import avec empreinte attendue et 9 tests preview passent. Les contrôles Svelte effectués ne signalent aucune erreur ni avertissement. La validation globale, le parcours matériel et les paquets 0.1.1 restent à vérifier.

La release 0.1.1 demandée comprend DEB, RPM et AppImage, reconstruits avec Ubuntu 22.04 pour conserver glibc 2.35. La publication attend les checks, le contrôle des installateurs et les téléchargements publics avec empreintes conformes.
