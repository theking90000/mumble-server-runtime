# Décision 0001 — Format UDP legacy : supporté ou non ?

**Statut : OUVERTE — à trancher par un humain avant de clore la Phase 0.**

La roadmap (Phase 0, livrable 4) exige que cette décision soit **gelée** : elle
conditionne la structure de `voxloom-protocol` (une seule enveloppe UDP protobuf
1.5+, ou deux chemins d'enveloppe) et ne doit pas rester ouverte pendant que le
codec s'écrit.

## Enjeu

- **Protobuf UDP (Mumble 1.5+)** : requis dans tous les cas.
- **Legacy UDP** (header varint, types audio, ping) : nécessaire pour les clients
  plus anciens et certains clients mobiles.

## Recommandation de la roadmap

> Legacy requis **si** un client Android doit se connecter un jour — et le corpus
> doit alors inclure une session Mumla (voir `fixtures/corpus/` scénario 09).

## Conséquences selon la décision

| Décision | `voxloom-protocol` | Corpus |
|----------|--------------------|--------|
| Legacy **non** | une seule enveloppe UDP (protobuf 1.5+) | 8 scénarios suffisent |
| Legacy **oui** | deux chemins d'enveloppe + décodage varint legacy | + scénario Mumla obligatoire |

## Décision retenue

_À compléter par l'humain. Renseigner : la décision, la date, la justification,
et cocher le scénario corpus correspondant._
