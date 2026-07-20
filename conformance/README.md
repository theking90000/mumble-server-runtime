# conformance/ — tests de conformité (zone vérificateur, R2)

> **Zone vérificateur (R2).** Modifiable uniquement par des tâches de vérification,
> et **toute modification passe par une revue humaine**. Un diff qui touche ce
> répertoire ET une implémentation (`voxloom-*/src`) est refusé par
> `ci/verifier-boundary.sh`.

Ce répertoire héberge les oracles de conformité protocolaire, remplis au fil des
phases :

- golden tests de la séquence initiale contre le corpus (Phase 3),
- assertions nommées des 20 invariants de la spec §20 (Phase 5),
- scénarios de non-fuite inter-realm (Phase 7),
- checklists humaines signées (Phases 2, 4, 6, 9).

Vide en Phase 0 : les oracles se construisent quand la phase qui les exige commence.
