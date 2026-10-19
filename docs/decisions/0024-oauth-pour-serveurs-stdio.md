# 0024 : un serveur MCP stdio peut s'autoriser par OAuth, jeton injecté au lancement

Statut : acceptée (19 octobre 2026). Portée : `penelope-mcp` (`oauth_issuer`,
`oauth_params`), `penelope-mcp-host` (autorisation, lancement, relance), `penelope-agenda-mcp`
(`AGENDA_TOKEN`). Issue #405, après #295 (décision [0018](0018-agenda-caldav-en-lecture.md))
et #403.

## Contexte

Le propriétaire veut ses agendas Google dans Pénélope. Le point CalDAV de Google exige
OAuth 2 ; notre serveur d'agenda ne connaissait que l'authentification Basic (0018).

Le serveur MCP officiel de Google Agenda est une préversion réservée aux inscrits, et le
propriétaire refuse les serveurs MCP communautaires. L'autorisation OAuth de Pénélope
existait déjà, mais seulement pour un serveur HTTP (Slack).

## Décision

1. **OAuth pour un serveur stdio, par déclaration.** `oauth_issuer` nomme le serveur
   d'autorisation. Le parcours est celui des serveurs HTTP (métadonnées RFC 8414 ou
   OpenID, PKCE S256, client configuré, lien sur Telegram, retour collé), sans découverte
   RFC 9728 et sans indicateur `resource`, puisqu'il n'y a pas de ressource.
2. **Le jeton passe par l'environnement.** `${OAUTH_ACCESS_TOKEN}` dans `env` est remplacé
   au lancement par un jeton rafraîchi. Il n'est écrit ni dans `mcp.d`, ni dans un journal,
   et il est enregistré pour le masquage.
3. **Relance avant expiration.** Le jeton ne se renouvelle pas dans un processus déjà
   lancé : une minute avant son expiration, le prochain appel relance le serveur avec un
   jeton neuf.
4. **Rien de propre à un fournisseur dans le code.** `oauth_params` porte ce qu'un
   fournisseur exige en plus (`access_type=offline` chez Google) ; ces paramètres
   s'ajoutent sans remplacer ceux du protocole.

## Raisons

- **Générique.** La même déclaration sert à tout service qui n'accepte qu'OAuth derrière
  un serveur stdio, pas seulement à Google.
- **Simple.** Injecter le jeton au lancement évite d'apprendre OAuth à chaque serveur, et
  évite de lui confier le jeton de rafraîchissement et le secret du client.

## Conséquences

- Plusieurs comptes chez un même fournisseur : une déclaration par compte, chacune avec
  son autorisation dans le SecretStore.
- Un serveur stdio sans autorisation reste en attente d'autorisation, et l'appel le dit.
