//! The `dependency` repository: one entity's outgoing edges, the transitive
//! closure the transient `gts-rust` store is built from, and the reverse-impact
//! read a revision refreshes against.
//!
//! # Two traversals over one table, in two different shapes
//!
//! [`DependencyRepo::closure`] walks forward (what a candidate consumes) with an
//! iterative worklist; [`DependencyRepo::reverse_impact`] walks backward (who
//! consumes the candidate) with one `WITH RECURSIVE`. The asymmetry is
//! deliberate, and it is not a portability story — `SecureCteSelect` (ADR-0001)
//! makes a scoped recursive CTE expressible through the typed builder on all
//! three backends, so both *could* be either shape.
//!
//! What separates them is what each caller needs back. The forward closure is
//! read to build a store, and its rows arrive as whole entities plus their
//! authored documents, which the walk interleaves with per-hop reads anyway.
//! The reverse read is a set of ids the commit is about to write to, and it runs
//! **inside the commit transaction** — where every extra round trip is time spent
//! holding the entity's row lock. One statement instead of two per hop is the
//! whole point.

use std::collections::{HashMap, HashSet};

use gts::GtsId;
use sea_orm::sea_query::{Alias, Expr, ExprTrait};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, Condition, EntityTrait, FromQueryResult, QueryFilter,
};
use toolkit_db::secure::{
    AccessScope, DBRunner, RecursiveCte, ScopeError, SecureDeleteExt, SecureEntityExt,
    SecureInsertManyExt,
};

use super::IN_CHUNK;
use super::entity_repo::EntityRepo;
use crate::domain::enums::DependencyKind;
use crate::domain::ports::{DependencyClosure, EntityRow, ReverseImpact};
use crate::infra::storage::entity::{dependency, entity};

/// Maximum size of one dependency closure.
///
/// **Its own bound, not `limits.activation_write_set`** — it took that key's value
/// (512) as a starting point and nothing more. The two count different things: this
/// bounds the entities one store build **reads**, SPEC §8.1 step 4.6's write set
/// bounds the dependents an admission **refreshes**. The earlier wording here said
/// this "mirrors" the key, which read as though an operator could move it; they
/// cannot, and the key's own documentation now says so (`config::Limits`).
///
/// A private constant is the honest shape while the number has no operator meaning:
/// the closure a single admission unit needs is bounded by its dependency graph
/// rather than by the entity count, and the measured max fan-out in-repo is well
/// under this. T14 is where a configured bound reaches this layer, and where the two
/// bounds should be told apart by name. Upgrade path if it is hit: the
/// generation/staging protocol in DESIGN §4.
const CLOSURE_BOUND: usize = 512;

/// Name of the reverse-impact CTE. A `&'static str` because that is what
/// `SecureCteSelect` takes, and one constant because the definition and the join
/// that references it must not drift apart.
const REVERSE_IMPACT_CTE: &str = "reverse_impact";

pub struct DependencyRepo;

impl DependencyRepo {
    /// Replace one entity's outgoing edges.
    ///
    /// Admission replaces only the admitted entity's outgoing rows, never anyone
    /// else's, so the delete is keyed on `from_entity_id` alone. Delete-then-insert
    /// rather than a diff: the edge set is small, and a diff would need to read the
    /// current rows to compute it.
    ///
    /// `edges` is treated as a **set**, because that is what the table is:
    /// `(from_entity_id, kind, to_entity_id)` is the primary key. Two references to
    /// the same base from one schema — a `$ref` used twice — are one edge, and
    /// leaving the duplicate in would turn an ordinary document into a primary-key
    /// violation partway through admission.
    pub async fn replace_outgoing(
        runner: &impl DBRunner,
        scope: &AccessScope,
        from_entity_id: i64,
        edges: &[(DependencyKind, i64)],
    ) -> Result<(), ScopeError> {
        dependency::Entity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(dependency::Column::FromEntityId.eq(from_entity_id)))
            .exec(runner)
            .await?;

        let mut seen: HashSet<(DependencyKind, i64)> = HashSet::with_capacity(edges.len());
        let unique: Vec<(DependencyKind, i64)> =
            edges.iter().filter(|e| seen.insert(**e)).copied().collect();
        if unique.is_empty() {
            return Ok(());
        }
        let rows = unique.iter().map(|(kind, to)| dependency::ActiveModel {
            from_entity_id: Set(from_entity_id),
            kind: Set((*kind).into()),
            to_entity_id: Set(*to),
        });
        for chunk in rows.collect::<Vec<_>>().chunks(IN_CHUNK) {
            dependency::Entity::insert_many(chunk.to_vec())
                .secure()
                // `dependency` carries no security dimension of its own: an edge is
                // reachable exactly when both endpoints are, and those rows are
                // scoped. There is nothing per-row to validate.
                .scope_unchecked(scope)?
                .exec(runner)
                .await?;
        }
        Ok(())
    }

    /// The entities directly consumed by any of `from_entity_ids`, chunked.
    pub async fn direct_dependencies(
        runner: &impl DBRunner,
        scope: &AccessScope,
        from_entity_ids: &[i64],
    ) -> Result<Vec<i64>, ScopeError> {
        let mut out = Vec::new();
        for chunk in from_entity_ids.chunks(IN_CHUNK) {
            let rows = dependency::Entity::find()
                .filter(dependency::Column::FromEntityId.is_in(chunk.iter().copied()))
                .secure()
                .scope_with(scope)
                .all(runner)
                .await?;
            out.extend(rows.into_iter().map(|r| r.to_entity_id));
        }
        Ok(out)
    }

    /// Candidate identifiers plus the transitive closure of what they consume,
    /// `gts_id`-sorted. This is what the transient `gts-rust` store is built from.
    ///
    /// An iterative worklist over direct edges rather than a recursive CTE — see
    /// the module header. The `seen` set is what keeps the walk linear in entities
    /// rather than in paths: the relation is acyclic (ADR-0012) but its paths
    /// converge, so a diamond would otherwise be re-walked once per path. It also
    /// bounds the damage if a row ever contradicted that invariant — bounded work
    /// instead of a walk that never ends.
    ///
    /// # The worklist is seeded from the identifier, not only from the edge table
    ///
    /// Each root contributes its whole `GtsId::chain_ids()` — every prefix of its
    /// `~`-chain — before the edge walk starts (T10). That is a **different
    /// relation**, not a shortcut for T13's stored edges: a derivation base is a
    /// pure function of the identifier (`base~derived~` consumes `base~` by being
    /// named so, and an Instance `base~thing.v1` conforms to `base~` likewise).
    /// Those relations are materialized for reverse impact, but validation seeds
    /// them from the identifier so a first admission does not depend on rows that
    /// are written only at commit. T13 also adds candidate `$ref` targets **as
    /// roots the caller passes in**, extracted from the candidate document. An
    /// `x-gts-ref` target is neither stored nor seeded because validation reads no
    /// target document. See `domain::gts_store::load_unit_store`.
    ///
    /// Candidates with no entity row are reported in
    /// [`DependencyClosure::missing_roots`] rather than failing the read, because a
    /// first admission's own candidate is exactly that case. **`missing_roots` is
    /// computed over the original roots only** — a chain member the seed added is
    /// not something the caller asked for, so its absence is the store builder's
    /// problem to name, not a missing root.
    pub async fn closure(
        runner: &impl DBRunner,
        scope: &AccessScope,
        roots: &[String],
    ) -> Result<DependencyClosure, ScopeError> {
        let mut seeds: Vec<String> = Vec::with_capacity(roots.len());
        for root in roots {
            match GtsId::try_new(root) {
                // Every prefix, which includes the root itself.
                Ok(id) => seeds.extend(id.chain_ids()),
                // An unparsable root is passed through unchanged so it lands in
                // `missing_roots` below, exactly as before. Refusing here would turn
                // a caller's bad identifier into a storage error.
                Err(_) => seeds.push(root.clone()),
            }
        }
        seeds.sort();
        seeds.dedup();

        let resolved = EntityRepo::find_by_gts_ids(runner, scope, &seeds).await?;
        let found: HashSet<&str> = resolved.iter().map(|r| r.gts_id.as_str()).collect();
        let mut missing_roots: Vec<String> = roots
            .iter()
            .filter(|r| !found.contains(r.as_str()))
            .cloned()
            .collect();
        missing_roots.sort();
        missing_roots.dedup();

        let mut seen: HashSet<i64> = resolved.iter().map(|r| r.id).collect();
        let mut frontier: Vec<i64> = seen.iter().copied().collect();
        let mut by_id: HashMap<i64, EntityRow> = resolved.into_iter().map(|r| (r.id, r)).collect();

        // The bound covers the seeded roots too, not only what the walk adds: a
        // batch of chained identifiers can already exceed it before the first
        // hop, and a first hop that discovers nothing new would break the loop
        // before the guard below ever runs.
        Self::ensure_within_bound(roots, by_id.len(), 0)?;

        while !frontier.is_empty() {
            let discovered = Self::direct_dependencies(runner, scope, &frontier).await?;
            let fresh: Vec<i64> = discovered
                .into_iter()
                .filter(|id| seen.insert(*id))
                .collect();
            if fresh.is_empty() {
                break;
            }
            Self::ensure_within_bound(roots, by_id.len(), fresh.len())?;
            for row in EntityRepo::find_by_ids(runner, scope, &fresh).await? {
                by_id.insert(row.id, row);
            }
            frontier = fresh;
        }

        let mut entities: Vec<EntityRow> = by_id.into_values().collect();
        entities.sort_by(|a, b| a.gts_id.cmp(&b.gts_id));
        Ok(DependencyClosure {
            entities,
            missing_roots,
        })
    }

    /// Refuse a closure that has exceeded or would exceed [`CLOSURE_BOUND`].
    ///
    /// Called once over the seeded roots (`newly_discovered = 0`) and once per
    /// hop over the entities that hop adds; the structured warning names the
    /// roots and the reached size so the operator can tell a wide graph from an
    /// oversized batch.
    ///
    /// # Errors
    /// [`ScopeError::Invalid`] when `resolved_entities + newly_discovered`
    /// exceeds the bound.
    fn ensure_within_bound(
        roots: &[String],
        resolved_entities: usize,
        newly_discovered: usize,
    ) -> Result<(), ScopeError> {
        if resolved_entities + newly_discovered > CLOSURE_BOUND {
            tracing::warn!(
                roots = ?roots,
                closure_bound = CLOSURE_BOUND,
                resolved_entities,
                newly_discovered,
                "types_registry dependency closure exceeded its safety bound"
            );
            return Err(ScopeError::Invalid(
                "dependency closure exceeds the 512-entity store-build bound; see the \
                 structured warning for roots and reached size",
            ));
        }
        Ok(())
    }

    /// Every entity that transitively depends on any of `roots`, `gts_id`-sorted,
    /// with the roots themselves excluded.
    ///
    /// One `WITH RECURSIVE` over `dependency` — see the module header for why this
    /// direction is a CTE and the forward closure is not. The recursion walks the
    /// edge table against itself: a row is followed when its `to_entity_id` equals a
    /// walked row's `from_entity_id`, which is the reverse of what the row says.
    /// The outer query joins `entity` so the caller gets rows rather than bare ids,
    /// and the join is a single indexed equality — never an `OR` across both
    /// endpoints, which is what makes `PostgreSQL` abandon the index
    /// (`SecureCteSelect::join_cte`).
    ///
    /// # ponytail: the write-set bound, and why it is also the depth cap
    ///
    /// `bound` is `limits.activation_write_set` (**512**; measured max fan-out
    /// in-repo is **27**). It is a refusal threshold, not a truncation point: over
    /// it, nothing is committed and the candidate fails with a structured reason.
    /// Upgrade path when a real graph reaches it: the generation/staging protocol in
    /// DESIGN §4, which stages the refresh instead of writing it in one transaction.
    ///
    /// The bound is *also* passed as the CTE's mandatory `max_depth`, and that pair
    /// is load-bearing. A depth cap truncates **silently** — a dependent left out of
    /// this set keeps stale artifacts marked current, which is the one failure mode
    /// this read must not have. Setting the cap to the bound makes truncation
    /// unobservable-but-harmless, because it cannot happen below the refusal:
    ///
    /// * Seed rows carry depth `0`, so a dependent whose shortest distance from a
    ///   root is `d` edges appears at depth `d - 1`, and the walk emits every
    ///   dependent within `bound + 1` edges.
    /// * If some dependent were hidden, its shortest path would be at least
    ///   `bound + 2` edges long, and every one of the `bound + 1` distinct
    ///   dependents along that path is *nearer* than it — so all of them are in
    ///   this set.
    /// * That set is then larger than `bound`, and the refusal below has already
    ///   fired.
    ///
    /// So a returned set is complete, and an incomplete walk is an error.
    ///
    /// # The relation is acyclic, and the walk still deduplicates
    ///
    /// No edge kind can close a cycle (ADR-0012): a circular `$ref` has no resolved
    /// form and GTS refuses it, derivation strictly shortens the `~`-chain, and
    /// nothing references an Instance. `UNION` is still the right union — a DAG's
    /// paths converge, and `UNION ALL` would enumerate a fan-in-heavy graph once per
    /// path. The depth cap then does double duty: it bounds the re-expansion that
    /// dedup-including-depth allows, and it keeps a row contradicting the acyclicity
    /// invariant from hanging the commit transaction this read runs inside.
    ///
    /// # Errors
    /// Propagates the read. Exceeding `bound` is [`ReverseImpact::OverBound`], not
    /// an error: it is a candidate refusal, and the caller words it as one.
    pub async fn reverse_impact(
        runner: &impl DBRunner,
        scope: &AccessScope,
        roots: &[i64],
        bound: usize,
    ) -> Result<ReverseImpact, ScopeError> {
        /// The projection: the dependent's entity id and nothing else. Narrowed
        /// because `SELECT DISTINCT` compares every selected column, and because the
        /// full rows are read once at the end rather than once per duplicate.
        #[derive(FromQueryResult)]
        struct DependentId {
            id: i64,
        }

        if roots.is_empty() {
            return Ok(ReverseImpact::Within(Vec::new()));
        }
        // `u32::MAX` is unreachable for any configured bound and would refuse below
        // long before the cap mattered; saturating keeps the cast from being a
        // fallible operation with no meaningful error.
        let max_depth = u32::try_from(bound).unwrap_or(u32::MAX);
        let read_limit = u64::try_from(bound.saturating_add(1)).unwrap_or(u64::MAX);

        let mut dependents: HashSet<i64> = HashSet::new();
        // Chunked for the same reason every other `IN (…)` here is: the seed
        // predicate binds one parameter per root. Reverse reachability is a union
        // over the roots, so a chunked read is the same answer as one read — and the
        // completeness argument above holds per chunk, since each chunk's own count
        // is checked against the same bound.
        for chunk in roots.chunks(IN_CHUNK) {
            let walk = RecursiveCte::<dependency::Entity>::new(
                REVERSE_IMPACT_CTE,
                Condition::all().add(dependency::Column::ToEntityId.is_in(chunk.iter().copied())),
                // The next row's `to_entity_id` points at a walked row's
                // `from_entity_id`: the dependent of a dependent.
                dependency::Column::ToEntityId,
                dependency::Column::FromEntityId,
                max_depth,
            );
            let rows = entity::Entity::find()
                .secure()
                .scope_with(scope)
                .with_ctes()
                .recursive_cte(walk)
                .join_cte(
                    REVERSE_IMPACT_CTE,
                    Condition::all().add(
                        Expr::col((Alias::new(REVERSE_IMPACT_CTE), Alias::new("from_entity_id")))
                            .equals((entity::Entity, entity::Column::Id)),
                    ),
                )
                // The roots are the candidates the commit refreshes itself.
                // Excluded here rather than in memory so the row limit below counts
                // only rows the caller will write to.
                .filter(Condition::all().add(entity::Column::Id.is_not_in(roots.iter().copied())))
                .select_only()
                .column(entity::Column::Id)
                // `join_cte` is an inner join and a dependent is reached by as many
                // rows as there are paths to it, so without this a hub-shaped graph
                // would return the same id many times over.
                .distinct()
                // One row past the bound is all the refusal needs to see.
                .limit(read_limit)
                .all_as::<DependentId>(runner)
                .await?;

            dependents.extend(rows.into_iter().map(|r| r.id));
            if dependents.len() > bound {
                return Ok(Self::over_write_set(roots, dependents.len(), bound));
            }
        }

        let mut ids: Vec<i64> = dependents.into_iter().collect();
        // Sorted so the follow-up read's `IN (…)` chunks are stable; the returned
        // order is `gts_id`, set below.
        ids.sort_unstable();
        let mut rows = EntityRepo::find_by_ids(runner, scope, &ids).await?;
        rows.sort_by(|a, b| a.gts_id.cmp(&b.gts_id));
        Ok(ReverseImpact::Within(rows))
    }

    /// Report a reverse-impact set larger than the write-set bound.
    ///
    /// A sibling of [`Self::ensure_within_bound`] guarding a different number:
    /// that one bounds the entities a store build **reads**, this one the
    /// dependents an admission **writes** (`config::Limits::activation_write_set`).
    /// Collapsing them would let an operator move one by editing the other.
    ///
    /// Returns the outcome rather than an error, and logs at `warn` here rather
    /// than at the caller: this is the layer that knows the roots and the size
    /// reached, and the candidate refusal the caller writes carries neither into
    /// the operator's logs.
    fn over_write_set(roots: &[i64], at_least: usize, bound: usize) -> ReverseImpact {
        tracing::warn!(
            roots = ?roots,
            activation_write_set = bound,
            at_least,
            "types_registry reverse-impact set exceeded the activation write set bound"
        );
        ReverseImpact::OverBound { at_least, bound }
    }
}
