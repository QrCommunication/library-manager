# Fournisseurs et catalogue de modèles

Les identifiants sont récupérés à la volée. Une erreur ne devient jamais une liste inventée. Le cache conserve sa provenance et sa date.

| Fournisseur | Catalogue | Chat |
|---|---|---|
| Z.ai | https://docs.z.ai/openapi.json : enum modèles des schémas ChatCompletionTextRequest et ChatCompletionVisionRequest | https://api.z.ai/api/paas/v4/chat/completions |
| Kimi | https://api.moonshot.ai/v1/models | /v1/chat/completions |
| MiniMax | https://api.minimax.io/v1/models | /v1/chat/completions |
| Mistral | https://api.mistral.ai/v1/models ; filtrer completion_chat | /v1/chat/completions |
| Claude | https://api.anthropic.com/v1/models ; pagination after_id | /v1/messages |
| Codex / OpenAI | https://api.openai.com/v1/models ; modèles de conversation et de code | https://api.openai.com/v1/responses |

Z.ai : catalogue public de documentation, ne prouve pas les permissions d’un compte. API générale uniquement ; ne pas détourner un abonnement Coding Plan. MiniMax : max_completion_tokens, conservation intégrale du message assistant entre appels d’outils. Claude : X-Api-Key et anthropic-version. Les autres API utilisent Bearer. Codex / OpenAI utilise Responses, sans installer de CLI ni service local.

Une clé API du fournisseur est requise pour les appels authentifiés. Les abonnements aux interfaces Web de ChatGPT/Codex ou Claude ne fournissent pas automatiquement une clé API ni du quota API. L’enregistrement permanent est optionnel et utilise le coffre natif du système (Secret Service sur Linux) ; s’il est absent ou verrouillé, l’erreur reste visible et les clés de session restent disponibles. Aucun secret n’est enregistré dans SQLite ou un fichier de paramètres.

Internet : recherches bibliographiques réalisées par Library Manager pour chaque fournisseur, URLs de provenance envoyées au modèle. Les outils supplémentaires sont limités à web_search et fetch_url. Aucun outil shell ou modification libre de fichiers.

Sources primaires vérifiées le 2026-10-09 :

- https://docs.z.ai/openapi.json
- https://docs.z.ai/devpack/usage-policy
- https://platform.kimi.ai/docs/api/list-models
- https://platform.kimi.ai/docs/guide/use-kimi-api-to-complete-tool-calls
- https://platform.minimax.io/docs/api-reference/models/openai/list-models
- https://platform.minimax.io/docs/guides/text-m3-function-call
- https://docs.mistral.ai/api/endpoint/models
- https://platform.claude.com/docs/en/api/models/list
- https://developers.openai.com/api/reference/resources/models/methods/list
- https://developers.openai.com/api/reference/resources/responses/methods/create
- https://docs.rs/keyring/4.2.0/keyring/v1/

Validation actuelle : catalogue Z.ai public interrogé réellement (21 modèles retournés le 2026-10-09). Neuf tests de contrat vérifient les six parsers, le cache, la pagination Claude, la sélection de modèle, les formats de requête/réponse, les changements de compte, les limites HTTP et la protection des clés. Les API payantes authentifiées nécessitent les clés du compte ; ces tests ne constituent pas une validation de quota ni une génération payante réelle.
