//! The `dependency` repository: one entity's outgoing edges, the transitive
//! closure the transient `gts-rust` store is built from, and the reverse-impact
//! read a revision refreshes against.
//!
//! # Two traversals over one table, one shape
//!
//! [`DependencyRepo::closure`] walks forward (what a candidate consumes) and
//! [`DependencyRepo::reverse_impact`] walks backward (who consumes the
//! candidate). Both are one scoped `WITH RECURSIVE` through `SecureCteSelect`
//! (ADR-0001), and they differ only in which of the edge table's two columns
//! anchors the recursion and which one the outer join reads back.
//!
//! The forward walk was an iterative worklist until this commit, on the reasoning
//! that its caller wants whole entities plus their authored documents and so
//! interleaves per-hop `entity` reads anyway. That reasoning was circular: the
//! per-hop read never drove the walk — the frontier came from the edge table
//! alone — so it was a batched read the loop had merely spread across hops.
//! Round trips are now fixed rather than linear in depth for both directions.
//!
//! What still differs is what a round trip *costs*. The reverse read runs
//! **inside the commit transaction**, where each one is time spent holding the
//! entity's row lock; the forward read runs in the caller's snapshot, where it is
//! only latency. That is a difference in how much the saving is worth, not in
//! whether the shape is available.

use std::collections::{HashMap, HashSet};

use gts::GtsId;
use sea_orm::sea_query::{Alias, Expr, ExprTrait};
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, FromQueryResult};
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

/// Name of the forward-closure CTE. A `&'static str` because that is what
/// `SecureCteSelect` takes, and one constant because the definition and the join
/// that references it must not drift apart.
const FORWARD_CLOSURE_CTE: &str = "forward_closure";

/// Name of the reverse-impact CTE. See [`FORWARD_CLOSURE_CTE`].
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

    /// Candidate identifiers plus the transitive closure of what they consume,
    /// `gts_id`-sorted. This is what the transient `gts-rust` store is built from.
    ///
    /// Three reads regardless of graph depth: resolve the seeds, one
    /// `WITH RECURSIVE` for everything reachable from them, one batched read for
    /// the rows the walk added. See the module header for what this replaced.
    ///
    /// # The walk is seeded from the identifier, not only from the edge table
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
    ///
    /// # Errors
    /// Propagates the reads, and [`ScopeError::Invalid`] when the closure exceeds
    /// [`CLOSURE_BOUND`].
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

        let seed_ids: Vec<i64> = resolved.iter().map(|r| r.id).collect();
        let mut by_id: HashMap<i64, EntityRow> = resolved.into_iter().map(|r| (r.id, r)).collect();

        // The bound covers the seeded roots on their own: a batch of chained
        // identifiers can exceed it before a single edge is followed, and the walk
        // below would then never run to check it.
        Self::ensure_within_bound(roots, by_id.len())?;

        // The walk returns everything reachable, which includes any seed another
        // seed consumes; those rows are already in hand, and `by_id` is the filter.
        let fresh: Vec<i64> = Self::forward_reachable(runner, scope, roots, &seed_ids)
            .await?
            .into_iter()
            .filter(|id| !by_id.contains_key(id))
            .collect();
        for row in EntityRepo::find_by_ids(runner, scope, &fresh).await? {
            by_id.insert(row.id, row);
        }

        let mut entities: Vec<EntityRow> = by_id.into_values().collect();
        entities.sort_by(|a, b| a.gts_id.cmp(&b.gts_id));
        Ok(DependencyClosure {
            entities,
            missing_roots,
        })
    }

    /// Every entity reachable from `seed_ids` by following outgoing edges, seeds
    /// included where one seed consumes another.
    ///
    /// One `WITH RECURSIVE` over `dependency`, the mirror image of
    /// [`Self::reverse_impact`]: a row is followed when its `from_entity_id` equals
    /// a walked row's `to_entity_id`, which is what the row already says, and the
    /// outer query joins `entity` on the `to_entity_id` side. The join is a single
    /// indexed equality for the same reason it is there — never an `OR` across both
    /// endpoints (`SecureCteSelect::join_cte`).
    ///
    /// # The bound is also the depth cap, and the two together rule out truncation
    ///
    /// [`CLOSURE_BOUND`] is passed as the CTE's mandatory `max_depth` and as the
    /// per-chunk row limit, exactly as `activation_write_set` is in
    /// [`Self::reverse_impact`], and the argument carries over unchanged. A depth
    /// cap truncates **silently**, and a dependency missing from the store is worse
    /// than a refusal: it would make the candidate fail validation for a reason that
    /// is not about the candidate. Seed rows carry depth `0`, so an entity whose
    /// shortest distance from a seed is `d` edges appears at depth `d - 1`, and the
    /// walk emits everything within `CLOSURE_BOUND + 1` edges. Anything hidden would
    /// sit at least `CLOSURE_BOUND + 2` edges out, behind `CLOSURE_BOUND + 1`
    /// distinct nearer entities that are all in this set — which is already over the
    /// bound, where the refusal has fired.
    ///
    /// The row limit is exact for the same reason. A chunk returning fewer rows
    /// than the limit was not truncated. One returning the limit returned
    /// `CLOSURE_BOUND + 1` *distinct* entity ids — `DISTINCT` runs before `LIMIT` —
    /// so the accumulated set is over the bound whether or not any of them were
    /// new, and the refusal fires before a truncated chunk can be believed.
    ///
    /// # Errors
    /// Propagates the read, and [`ScopeError::Invalid`] past [`CLOSURE_BOUND`].
    async fn forward_reachable(
        runner: &impl DBRunner,
        scope: &AccessScope,
        roots: &[String],
        seed_ids: &[i64],
    ) -> Result<Vec<i64>, ScopeError> {
        /// The projection: the dependency's entity id and nothing else. Narrowed
        /// because `SELECT DISTINCT` compares every selected column, and because the
        /// full rows are read once at the end rather than once per duplicate.
        #[derive(FromQueryResult)]
        struct DependencyId {
            id: i64,
        }

        if seed_ids.is_empty() {
            return Ok(Vec::new());
        }
        // `u32::MAX` is unreachable for this bound and would refuse below long
        // before the cap mattered; saturating keeps the cast infallible.
        let max_depth = u32::try_from(CLOSURE_BOUND).unwrap_or(u32::MAX);
        let read_limit = u64::try_from(CLOSURE_BOUND.saturating_add(1)).unwrap_or(u64::MAX);

        // Seeded with the roots because the bound counts the whole closure, not only
        // what the walk adds, and because a seed reached through another seed must
        // not be counted twice.
        let mut reachable: HashSet<i64> = seed_ids.iter().copied().collect();
        // Chunked for the same reason every other `IN (…)` here is: the seed
        // predicate binds one parameter per root. Forward reachability is a union
        // over the seeds, so a chunked read is the same answer as one read — and the
        // no-truncation argument above holds per chunk, since each chunk's limit is
        // the same bound the accumulated set is checked against.
        for chunk in seed_ids.chunks(IN_CHUNK) {
            let walk = RecursiveCte::<dependency::Entity>::new(
                FORWARD_CLOSURE_CTE,
                Condition::all().add(dependency::Column::FromEntityId.is_in(chunk.iter().copied())),
                // The next row's `from_entity_id` points at a walked row's
                // `to_entity_id`: the dependency of a dependency.
                dependency::Column::FromEntityId,
                dependency::Column::ToEntityId,
                max_depth,
            );
            let rows = entity::Entity::find()
                .secure()
                .scope_with(scope)
                .with_ctes()
                .recursive_cte(walk)
                .join_cte(
                    FORWARD_CLOSURE_CTE,
                    Condition::all().add(
                        Expr::col((Alias::new(FORWARD_CLOSURE_CTE), Alias::new("to_entity_id")))
                            .equals((entity::Entity, entity::Column::Id)),
                    ),
                )
                // The seeds are *not* excluded in SQL, unlike the reverse read's
                // roots: there can be `CLOSURE_BOUND` of them, and a `NOT IN` that
                // wide would push the statement past `SQLite`'s parameter limit —
                // which is what `IN_CHUNK` exists to stay under. They are absorbed
                // by the set instead, at no cost to the limit's exactness.
                .select_only()
                .column(entity::Column::Id)
                // `join_cte` is an inner join and an entity is reached by as many
                // rows as there are paths to it, so without this a diamond would
                // return the same id once per path.
                .distinct()
                // One row past the bound is all the refusal needs to see.
                .limit(read_limit)
                .all_as::<DependencyId>(runner)
                .await?;

            reachable.extend(rows.into_iter().map(|r| r.id));
            Self::ensure_within_bound(roots, reachable.len())?;
        }

        let mut ids: Vec<i64> = reachable.into_iter().collect();
        // Sorted so the follow-up read's `IN (…)` chunks are stable.
        ids.sort_unstable();
        Ok(ids)
    }

    /// Refuse a closure that has exceeded [`CLOSURE_BOUND`].
    ///
    /// Called over the seeded roots before the walk, and again after each chunk of
    /// the walk; the structured warning names the roots and the reached size so the
    /// operator can tell a wide graph from an oversized batch.
    ///
    /// # Errors
    /// [`ScopeError::Invalid`] when `reached` exceeds the bound.
    fn ensure_within_bound(roots: &[String], reached: usize) -> Result<(), ScopeError> {
        if reached > CLOSURE_BOUND {
            tracing::warn!(
                roots = ?roots,
                closure_bound = CLOSURE_BOUND,
                reached,
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
