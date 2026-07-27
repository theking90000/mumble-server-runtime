# Décision 0002 : l'état métier appartient au flavor

**Statut : ACCEPTÉE le 2026-07-26.**

## Contexte

La première roadmap attribuait à Voxloom un `CanonicalState` contenant joueurs,
realms, équipes, rôles, positions et radios. Cette forme séparait bien le métier
du protocole Mumble, mais pas le métier du runtime vocal. Elle aurait obligé le
cœur à connaître les concepts d'une application de jeu avant même
l'intégration Minecraft.

Voxloom doit pouvoir être compilé avec un flavor Minecraft UHC, un autre jeu ou
une application non ludique sans que ses crates centrales changent de modèle.

## Décision

Voxloom ne possède jamais l'état métier. Un **flavor** est une intégration
compilée avec le runtime qui possède :

- son type de snapshot métier ;
- ses identifiants, commandes et transactions ;
- sa sérialisation, son batching et sa concurrence ;
- les règles qui transforment un snapshot en sorties vocales.

Le runtime Voxloom possède uniquement son état vocal :

- connexions et sessions ;
- vues engagées et mappings d'IDs locaux ;
- files de sortie et transitions ;
- snapshots de routage audio ;
- générations publiées et références d'interaction.

Le contrat de flavor reste statique et minimal. Conceptuellement :

```rust
trait VoiceFlavor: Send + Sync + 'static {
    type Snapshot: Send + Sync + 'static;

    fn render(
        &self,
        snapshot: &Self::Snapshot,
        connection: ConnectionId,
    ) -> Result<RenderOutput, FlavorError>;
}
```

Le snapshot est immuable pendant un rendu et opaque pour Voxloom. Le flavor
publie une nouvelle révision quand son état change. Voxloom rend les connexions
concernées, valide les sorties, puis publie vues et audio dans l'ordre de
sécurité. P7 rerend toutes les connexions à chaque publication. Une invalidation
ciblée reste une optimisation P10 et doit conserver ce rendu complet comme
fallback.

Les sorties du flavor utilisent des clés sémantiques et des `ConnectionId`.
Voxloom reste seul propriétaire des `ChannelId`, `SessionId` et mappings
numériques propres à chaque vue.

Les actions entrantes deviennent des `VoiceEvent` contenant uniquement des
références vocales déjà résolues et une génération. Le flavor décide si
l'événement produit une mutation métier et, le cas échéant, publie un nouveau
snapshot. Voxloom n'applique aucune commande métier.

## Composition

La dépendance pointe toujours du flavor vers Voxloom :

```text
application métier
        |
flavor compilé
        |
        v
API de flavor Voxloom
        |
runtime vocal
```

Un binaire de composition choisit le flavor à la compilation. Aucune ABI de
plugin dynamique ni registre global de callbacks n'est requis.

## Conséquences

- Le crate `voxloom-state` prévu pour l'état canonique métier est retiré de la
  roadmap du cœur.
- `voxloom-control` coordonne les publications vocales ; il ne sérialise pas les
  commandes métier.
- Les concepts joueur, UUID, partie, realm, équipe, rôle, dimension, position,
  radio et proximité appartiennent aux flavors.
- Le scénario Aurora/Borealis devient un flavor de référence hors du cœur.
- P8 fournit le premier flavor de production Minecraft, sans ajouter de
  dépendance Minecraft aux crates centrales.
- Les opérations `MergeRealms`, `SplitRealm` et les acteurs par partie sont des
  choix du flavor, pas des primitives Voxloom.
- Les tests de confidentialité du cœur portent sur les sorties de deux
  générations arbitraires. Les invariants métier restent testés dans le flavor.

## Non-objectifs

- construire un framework de plugins dynamiques ;
- définir un ECS générique ;
- standardiser toutes les commandes métier possibles ;
- sérialiser un snapshot opaque dans le hot path audio ;
- optimiser l'invalidation avant mesure.
