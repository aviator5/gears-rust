//! The `dependency` repository: one entity's outgoing edges, the transitive closure the transient
//! `gts-rust` store is built from, and the reverse-impact read a revision refreshes against.

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
/// Independent of `limits.activation_write_set`: this limits reads, not refreshed rows.
const CLOSURE_BOUND: usize = 512;

/// Name of the forward-closure CTE.
const FORWARD_CLOSURE_CTE: &str = "forward_closure";

/// Name of the reverse-impact CTE.
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

    /// Candidate identifiers and their transitive dependencies, sorted by `gts_id`.
    /// Identifier-chain prefixes and caller-supplied `$ref` targets seed one recursive CTE.
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

        // The bound covers the seeded roots on their own: a batch of chained identifiers can exceed
        // it before a single edge is followed, and the walk below would then never run to check it.
        Self::ensure_within_bound(roots, by_id.len())?;

        // The walk returns everything reachable, which includes any seed another seed consumes;
        // those rows are already in hand, and `by_id` is the filter.
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

    /// Follow outgoing edges from `seed_ids`, including reachable seeds.
    /// The CTE reads one row past [`CLOSURE_BOUND`] so depth or row truncation becomes an error.
    ///
    /// # Errors
    /// Propagates the read, and [`ScopeError::Invalid`] past [`CLOSURE_BOUND`].
    async fn forward_reachable(
        runner: &impl DBRunner,
        scope: &AccessScope,
        roots: &[String],
        seed_ids: &[i64],
    ) -> Result<Vec<i64>, ScopeError> {
        /// The projection: the dependency's entity id and nothing else.
        #[derive(FromQueryResult)]
        struct DependencyId {
            id: i64,
        }

        if seed_ids.is_empty() {
            return Ok(Vec::new());
        }
        // `u32::MAX` is unreachable for this bound and would refuse below long before the cap
        // mattered; saturating keeps the cast infallible.
        let max_depth = u32::try_from(CLOSURE_BOUND).unwrap_or(u32::MAX);
        let read_limit = u64::try_from(CLOSURE_BOUND.saturating_add(1)).unwrap_or(u64::MAX);

        // Seeded with the roots because the bound counts the whole closure, not only what the walk
        // adds, and because a seed reached through another seed must not be counted twice.
        let mut reachable: HashSet<i64> = seed_ids.iter().copied().collect();
        // Chunked for the same reason every other `IN (…)` here is: the seed predicate binds one
        // parameter per root.
        for chunk in seed_ids.chunks(IN_CHUNK) {
            let walk = RecursiveCte::<dependency::Entity>::new(
                FORWARD_CLOSURE_CTE,
                Condition::all().add(dependency::Column::FromEntityId.is_in(chunk.iter().copied())),
                // The next row's `from_entity_id` points at a walked row's `to_entity_id`: the
                // dependency of a dependency.
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
                // The seeds are *not* excluded in SQL, unlike the reverse read's roots: there can
                // be `CLOSURE_BOUND` of them, and a `NOT IN` that wide would push the statement
                // past `SQLite`'s parameter limit — which is what `IN_CHUNK` exists to stay under.
                .select_only()
                .column(entity::Column::Id)
                // `join_cte` is an inner join and an entity is reached by as many rows as there are
                // paths to it, so without this a diamond would return the same id once per path.
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

    /// Every entity that transitively depends on any of `roots`, `gts_id`-sorted, with the roots
    /// themselves excluded.
    pub async fn reverse_impact(
        runner: &impl DBRunner,
        scope: &AccessScope,
        roots: &[i64],
        bound: usize,
    ) -> Result<ReverseImpact, ScopeError> {
        /// The projection: the dependent's entity id and nothing else.
        #[derive(FromQueryResult)]
        struct DependentId {
            id: i64,
        }

        if roots.is_empty() {
            return Ok(ReverseImpact::Within(Vec::new()));
        }
        // `u32::MAX` is unreachable for any configured bound and would refuse below long before the
        // cap mattered; saturating keeps the cast from being a fallible operation with no
        // meaningful error.
        let max_depth = u32::try_from(bound).unwrap_or(u32::MAX);
        let read_limit = u64::try_from(bound.saturating_add(1)).unwrap_or(u64::MAX);

        let mut dependents: HashSet<i64> = HashSet::new();
        // Chunked for the same reason every other `IN (…)` here is: the seed predicate binds one
        // parameter per root.
        for chunk in roots.chunks(IN_CHUNK) {
            let walk = RecursiveCte::<dependency::Entity>::new(
                REVERSE_IMPACT_CTE,
                Condition::all().add(dependency::Column::ToEntityId.is_in(chunk.iter().copied())),
                // The next row's `to_entity_id` points at a walked row's `from_entity_id`: the
                // dependent of a dependent.
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
                .filter(Condition::all().add(entity::Column::Id.is_not_in(roots.iter().copied())))
                .select_only()
                .column(entity::Column::Id)
                // `join_cte` is an inner join and a dependent is reached by as many rows as there
                // are paths to it, so without this a hub-shaped graph would return the same id many
                // times over.
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
        // Sorted so the follow-up read's `IN (…)` chunks are stable; the returned order is
        // `gts_id`, set below.
        ids.sort_unstable();
        let mut rows = EntityRepo::find_by_ids(runner, scope, &ids).await?;
        rows.sort_by(|a, b| a.gts_id.cmp(&b.gts_id));
        Ok(ReverseImpact::Within(rows))
    }

    /// Report a reverse-impact set larger than the write-set bound.
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
