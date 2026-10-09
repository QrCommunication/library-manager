# Analyse d’impact initiale

2026-10-09 — projet neuf, aucun historique à migrer. Analyse indépendante avant code, risque global moyen (5/10), aucun blocage de développement.

## Protections obligatoires

- Import sur copies : originaux immuables, hash SHA-256, chemins gérés par le backend.
- Archives bornées : refus des traversées, doublons, expansions excessives, DTD XML, contenu DRM à transformer.
- Lecteur isolé : contenu sans script, frame sandbox, aucune ressource réseau ou commande native accessible.
- Métadonnées IA : schéma strict, provenance, confiance, contrôle ISBN, conservation des informations existantes sans preuve.
- Appareils : identité du montage revalidée avant écriture, staging sur même volume, contrôle checksum, aucune suppression implicite.
- CrossPoint : préserver progression/settings/fonts, index marqué sale après transfert.
- Internet : outils bibliographiques uniquement, protections SSRF/DNS/redirections, délais et tailles bornés.
- Secrets : keyring système ou mémoire de session ; aucune clé dans SQLite ou logs.
- Standalone : aucun appel à un Calibre externe, conversions natives ou moteur embarqué.

## Validation

Fixtures EPUB 2/3, séries décimales, traducteurs, Unicode, import idempotent ; attaques ZIP/XML/XHTML ; retrait de montage et panne de transfert ; conservation texte et spine après optimisation ; formats convertis sans Calibre ; erreurs et pagination des catalogues IA ; SSRF IPv4/IPv6 ; démarrage hors ligne ; FR/EN et langue ajoutée ; vrais binaires et paquets Linux.

Les tests simulés, les intégrations authentifiées et les vérifications matérielles sont distingués dans le rapport de livraison. Les livres personnels ne sont jamais publiés.

Source normative : https://www.w3.org/TR/epub-33/
