# Langues et traductions / Languages and translations

L’application propose le français et l’anglais. Le choix `system` normalise la langue du système : `fr-FR` et `fr-CA` utilisent `fr`, `en-GB` utilise `en`. Une langue non disponible utilise l’anglais. Le réglage utilisateur reste prioritaire sur le système.

Les textes de l’interface sont dans `src/lib/locales/fr.json` et `src/lib/locales/en.json`. Les dates, nombres et tailles utilisent les fonctions de formatage centralisées. Les titres des livres, notes personnelles et textes importés ne sont pas traduits automatiquement.

## Ajouter une langue

1. Copier `src/lib/locales/en.json` vers un fichier portant un code BCP 47, par exemple `de.json` ou `pt-BR.json`.
2. Traduire les valeurs en conservant les clés, les variables entre accolades et la structure des pluriels.
3. Vérifier le code de langue du nom de fichier : le sélecteur affiche automatiquement son nom avec `Intl.DisplayNames`.
4. Exécuter `pnpm check` et `pnpm test` pour vérifier le typage et la cohérence des traductions.
5. Vérifier visuellement les menus, dialogues, messages d’erreur et textes longs.

Les dictionnaires sont découverts par `import.meta.glob` dans `i18n.ts`. Il n’est pas nécessaire d’ajouter une branche dans chaque composant pour rendre une nouvelle langue sélectionnable. Les clés absentes utilisent le texte anglais de secours.

## English

The default language follows the system locale, with English as fallback. An explicit language setting takes precedence. Add a locale JSON file under `src/lib/locales/`, preserve the English keys and interpolation variables, then run the checks above. Books and personal notes are not automatically translated.
