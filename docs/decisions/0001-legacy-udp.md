# Décision 0001 — Format UDP legacy : supporté ou non ?

**Statut : TRANCHÉE le 2026-07-20 — legacy structurellement prévu, non implémenté
pour l'instant.**

## Décision

`voxloom-protocol` est conçu pour **deux chemins d'enveloppe UDP** (protobuf 1.5+
et legacy varint) : l'abstraction d'enveloppe laisse la place au legacy, de sorte
qu'il puisse être ajouté plus tard sans refonte. **Seul le chemin protobuf est
implémenté maintenant** ; le legacy n'est pas réalisé, car potentiellement inutile.
S'il s'avère nécessaire (p. ex. un client mobile/Mumla à supporter), il sera ajouté
à ce moment-là, en réactivant `docs/decisions/` et le scénario corpus Mumla.

**Conséquences pratiques :**
- L'implémentation ne se **bloque pas** en attendant le legacy : le chemin non
  implémenté reste fail-closed (R6), pas un `todo!()` atteignable.
- Le corpus des 8 scénarios de base suffit pour clore la Phase 0 ; le scénario
  Mumla (`09-mumla-legacy-udp`) reste optionnel, requis seulement si/quand le
  legacy est effectivement implémenté.

---

## Contexte de la décision (conservé)

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

