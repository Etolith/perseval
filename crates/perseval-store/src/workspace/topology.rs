use super::*;

const TOPOLOGY_PROJECTION_VERSION: i64 = 2;

#[derive(Default)]
pub(super) struct LiveTopologyCache {
    epoch: u64,
    indexes: HashMap<(String, u64), LiveTopologyIndex>,
}

#[derive(Default)]
struct LiveTopologyIndex {
    parents: HashMap<String, Option<String>>,
    child_counts: HashMap<String, usize>,
    depth_cache: HashMap<String, (u64, u32)>,
    generation: u64,
}

impl LiveTopologyIndex {
    fn from_rows(rows: Vec<(String, Option<String>)>) -> Self {
        let mut index = Self::default();
        index.apply(rows);
        index
    }

    fn apply(&mut self, rows: impl IntoIterator<Item = (String, Option<String>)>) {
        let mut changed = false;
        for (span_id, parent_span_id) in rows {
            let previous = self.parents.insert(span_id.clone(), parent_span_id.clone());
            if previous.as_ref() == Some(&parent_span_id) {
                continue;
            }
            if let Some(previous_parent) = previous.flatten()
                && let Some(count) = self.child_counts.get_mut(&previous_parent)
            {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.child_counts.remove(&previous_parent);
                }
            }
            if let Some(parent_span_id) = parent_span_id {
                *self.child_counts.entry(parent_span_id).or_default() += 1;
            }
            changed = true;
        }
        if changed {
            self.generation = self.generation.wrapping_add(1);
        }
    }

    fn annotation(&mut self, span_id: &str) -> (u32, bool) {
        let depth = self.depth(span_id);
        let has_children = self.child_counts.get(span_id).copied().unwrap_or_default() > 0;
        (depth, has_children)
    }

    fn depth(&mut self, span_id: &str) -> u32 {
        if let Some((generation, depth)) = self.depth_cache.get(span_id).copied()
            && generation == self.generation
        {
            return depth;
        }

        let mut path = Vec::new();
        let mut positions = HashMap::new();
        let mut current = span_id.to_owned();
        let mut known_parent_depth = None;
        let mut cyclic = false;
        loop {
            if let Some((generation, depth)) = self.depth_cache.get(&current).copied()
                && generation == self.generation
            {
                known_parent_depth = Some(depth);
                break;
            }
            if positions.insert(current.clone(), path.len()).is_some() {
                cyclic = true;
                break;
            }
            let Some(parent) = self.parents.get(&current).cloned() else {
                break;
            };
            path.push(current);
            let Some(parent) = parent else {
                break;
            };
            current = parent;
        }

        if cyclic {
            for member in path {
                self.depth_cache.insert(member, (self.generation, 0));
            }
        } else {
            let mut parent_depth = known_parent_depth;
            for member in path.into_iter().rev() {
                let depth = parent_depth.map_or(0, |depth| depth.saturating_add(1));
                self.depth_cache.insert(member, (self.generation, depth));
                parent_depth = Some(depth);
            }
        }
        self.depth_cache
            .get(span_id)
            .map(|(_, depth)| *depth)
            .unwrap_or_default()
    }
}

pub(super) fn recover_topology_jobs(control: &SqliteConnection) -> Result<(), StoreError> {
    control.execute(
        "UPDATE trace_revisions
            SET topology_status = 'pending', topology_last_error = NULL
          WHERE lifecycle = 'finalized'
            AND (topology_status != 'ready'
                 OR topology_projection_version IS NULL
                 OR topology_projection_version != ?1)",
        params![TOPOLOGY_PROJECTION_VERSION],
    )?;
    Ok(())
}

pub(super) fn load_topology(
    analytics: &DuckConnection,
    trace_id: &str,
    revision: u64,
) -> Result<Vec<(String, Option<String>)>, StoreError> {
    let mut statement = analytics.prepare(
        "SELECT span_id, parent_span_id FROM spans
         WHERE logical_trace_id = ?1 AND revision = ?2 AND is_current = TRUE
         ORDER BY start_time_unix_nano, span_id",
    )?;
    statement
        .query_map(duck_params![trace_id, revision as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .map(|row| row.map_err(StoreError::from))
        .collect()
}

pub(super) fn has_persisted_topology(
    analytics: &DuckConnection,
    trace_id: &str,
    revision: u64,
) -> Result<bool, StoreError> {
    let (span_count, projected_count): (i64, i64) = analytics.query_row(
        "SELECT COUNT(*), COUNT(CASE WHEN topology_order IS NOT NULL
                                      AND topology_projection_version = ?3 THEN 1 END) FROM spans
         WHERE logical_trace_id = ?1 AND revision = ?2 AND is_current = TRUE",
        duck_params![trace_id, revision as i64, TOPOLOGY_PROJECTION_VERSION],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(span_count > 0 && span_count == projected_count)
}

impl WorkspaceStore {
    pub(super) fn update_live_topology_indexes(
        &self,
        trace_revisions: &HashMap<String, (u64, bool)>,
        spans: &[SpanUpsertV1],
        touched: &BTreeMap<String, BTreeSet<String>>,
    ) {
        let mut updates = HashMap::<(String, u64), Vec<(String, Option<String>)>>::new();
        for span in spans {
            if !touched
                .get(&span.logical_trace_id)
                .is_some_and(|span_ids| span_ids.contains(&span.external_span_id))
            {
                continue;
            }
            let Some((revision, _)) = trace_revisions.get(&span.logical_trace_id) else {
                continue;
            };
            updates
                .entry((span.logical_trace_id.clone(), *revision))
                .or_default()
                .push((
                    span.external_span_id.clone(),
                    span.external_parent_span_id.clone(),
                ));
        }
        if updates.is_empty() {
            return;
        }
        let mut cache = self
            .live_topologies
            .lock()
            .expect("live topology cache lock poisoned");
        cache.epoch = cache.epoch.wrapping_add(1);
        for (key, rows) in updates {
            if let Some(index) = cache.indexes.get_mut(&key) {
                index.apply(rows);
            }
        }
    }

    pub(super) fn live_topology_annotations(
        &self,
        analytics: &DuckConnection,
        trace_id: &str,
        revision: u64,
        span_ids: &[String],
    ) -> Result<Vec<(u32, bool)>, StoreError> {
        let key = (trace_id.to_owned(), revision);
        loop {
            let epoch = {
                let mut cache = self
                    .live_topologies
                    .lock()
                    .expect("live topology cache lock poisoned");
                if let Some(index) = cache.indexes.get_mut(&key) {
                    return Ok(span_ids
                        .iter()
                        .map(|span_id| index.annotation(span_id))
                        .collect());
                }
                cache.epoch
            };
            let rows = load_topology(analytics, trace_id, revision)?;
            let mut cache = self
                .live_topologies
                .lock()
                .expect("live topology cache lock poisoned");
            if cache.epoch != epoch {
                continue;
            }
            let index = cache
                .indexes
                .entry(key.clone())
                .or_insert_with(|| LiveTopologyIndex::from_rows(rows));
            return Ok(span_ids
                .iter()
                .map(|span_id| index.annotation(span_id))
                .collect());
        }
    }

    pub fn claim_pending_topology(&self) -> Result<Option<TopologyProjectionJobV1>, StoreError> {
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let job = transaction
            .query_row(
                "SELECT logical_trace_id, revision FROM trace_revisions
                  WHERE lifecycle = 'finalized' AND topology_status = 'pending'
                  ORDER BY finalized_at_unix_ms, logical_trace_id, revision LIMIT 1",
                [],
                |row| {
                    Ok(TopologyProjectionJobV1 {
                        logical_trace_id: row.get(0)?,
                        revision: row.get::<_, i64>(1)?.max(0) as u64,
                    })
                },
            )
            .optional()?;
        if let Some(job) = &job {
            transaction.execute(
                "UPDATE trace_revisions
                    SET topology_status = 'running', topology_last_error = NULL,
                        topology_updated_at_unix_ms = ?1
                  WHERE logical_trace_id = ?2 AND revision = ?3
                    AND topology_status = 'pending'",
                params![now_unix_ms(), job.logical_trace_id, job.revision as i64],
            )?;
        }
        transaction.commit()?;
        Ok(job)
    }

    pub fn build_topology_projection(
        &self,
        job: &TopologyProjectionJobV1,
    ) -> Result<Vec<TopologyProjectionRowV1>, StoreError> {
        // Topology computation can scan a large finalized trace. Keep it off the sole writer
        // connection so ingestion and projection remain available while the read-only worker
        // builds the stable order in memory.
        let analytics = self.analytics_reads.connection();
        let topology = load_topology(&analytics, &job.logical_trace_id, job.revision)?;
        drop(analytics);
        let (ordered_span_ids, depths, parents_with_children) = topology_projection(&topology);
        Ok(ordered_span_ids
            .into_iter()
            .enumerate()
            .map(|(order, span_id)| TopologyProjectionRowV1 {
                depth: depths.get(&span_id).copied().unwrap_or_default(),
                has_children: parents_with_children.contains(&span_id),
                span_id,
                order: order as u64,
            })
            .collect())
    }

    pub fn commit_topology_chunk(
        &self,
        job: &TopologyProjectionJobV1,
        rows: &[TopologyProjectionRowV1],
        first: bool,
        last: bool,
    ) -> Result<Option<TraceDeltaV1>, StoreError> {
        let analytics = self
            .analytics
            .lock()
            .expect("analytics store lock poisoned");
        analytics.execute_batch("BEGIN TRANSACTION")?;
        let result = (|| -> Result<(), StoreError> {
            if first {
                analytics.execute(
                    "UPDATE spans SET topology_order = NULL, topology_depth = NULL,
                        topology_has_children = NULL, topology_projection_version = NULL
                      WHERE logical_trace_id = ?1 AND revision = ?2 AND is_current = TRUE",
                    duck_params![job.logical_trace_id, job.revision as i64],
                )?;
            }
            if !rows.is_empty() {
                analytics.execute_batch(
                    "CREATE TEMP TABLE topology_chunk_updates(
                        span_id VARCHAR PRIMARY KEY,
                        topology_order BIGINT NOT NULL,
                        topology_depth BIGINT NOT NULL,
                        topology_has_children BOOLEAN NOT NULL,
                        topology_projection_version BIGINT NOT NULL
                     )",
                )?;
                {
                    let mut appender = analytics.appender("topology_chunk_updates")?;
                    for row in rows {
                        appender.append_row(duck_params![
                            row.span_id,
                            row.order as i64,
                            row.depth as i64,
                            row.has_children,
                            TOPOLOGY_PROJECTION_VERSION,
                        ])?;
                    }
                }
                analytics.execute(
                    "UPDATE spans AS span SET
                        topology_order = topology.topology_order,
                        topology_depth = topology.topology_depth,
                        topology_has_children = topology.topology_has_children,
                        topology_projection_version = topology.topology_projection_version
                     FROM topology_chunk_updates AS topology
                     WHERE span.logical_trace_id = ?1 AND span.revision = ?2
                       AND span.span_id = topology.span_id AND span.is_current = TRUE",
                    duck_params![job.logical_trace_id, job.revision as i64],
                )?;
                analytics.execute_batch("DROP TABLE topology_chunk_updates")?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = analytics.execute_batch("ROLLBACK");
            return Err(error);
        }
        analytics.execute_batch("COMMIT")?;
        if last
            && !rows.is_empty()
            && !has_persisted_topology(&analytics, &job.logical_trace_id, job.revision)?
        {
            return Err(StoreError::Invalid(format!(
                "topology projection for {} revision {} is incomplete",
                job.logical_trace_id, job.revision
            )));
        }
        drop(analytics);

        if last {
            let mut control = self.control.lock().expect("control store lock poisoned");
            let transaction = control.transaction()?;
            let updated = transaction.execute(
                "UPDATE trace_revisions
                    SET topology_status = 'ready', topology_projection_version = ?1,
                        topology_last_error = NULL, topology_updated_at_unix_ms = ?2
                  WHERE logical_trace_id = ?3 AND revision = ?4
                    AND topology_status = 'running'",
                params![
                    TOPOLOGY_PROJECTION_VERSION,
                    now_unix_ms(),
                    job.logical_trace_id,
                    job.revision as i64,
                ],
            )?;
            if updated != 1 {
                return Err(StoreError::Invalid(format!(
                    "topology job for {} revision {} is not running",
                    job.logical_trace_id, job.revision
                )));
            }
            let current_revision = transaction
                .query_row(
                    "SELECT revision FROM logical_traces WHERE logical_trace_id = ?1",
                    params![job.logical_trace_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            let delta = if current_revision == Some(job.revision as i64) {
                let summary =
                    query_run_transaction(&transaction, &self.workspace_id, &job.logical_trace_id)?
                        .ok_or_else(|| StoreError::Invalid("topology trace disappeared".into()))?;
                Some(insert_delta_transaction(
                    &transaction,
                    &self.workspace_id,
                    summary,
                    TraceChangeKind::TopologyCommitted,
                    Vec::new(),
                )?)
            } else {
                None
            };
            transaction.commit()?;
            let mut cache = self
                .live_topologies
                .lock()
                .expect("live topology cache lock poisoned");
            cache.epoch = cache.epoch.wrapping_add(1);
            cache
                .indexes
                .remove(&(job.logical_trace_id.clone(), job.revision));
            return Ok(delta);
        }
        Ok(None)
    }

    pub fn fail_topology_projection(
        &self,
        job: &TopologyProjectionJobV1,
        error: &str,
    ) -> Result<(), StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        control.execute(
            "UPDATE trace_revisions
                SET topology_status = 'pending', topology_last_error = ?1,
                    topology_updated_at_unix_ms = ?2
              WHERE logical_trace_id = ?3 AND revision = ?4",
            params![
                truncate_error(error),
                now_unix_ms(),
                job.logical_trace_id,
                job.revision as i64,
            ],
        )?;
        Ok(())
    }

    pub fn topology_counts(&self) -> Result<(u64, u64), StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        control
            .query_row(
                "SELECT COUNT(CASE WHEN topology_status = 'pending' THEN 1 END),
                        COUNT(CASE WHEN topology_status = 'running' THEN 1 END)
                   FROM trace_revisions WHERE lifecycle = 'finalized'",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?.max(0) as u64,
                        row.get::<_, i64>(1)?.max(0) as u64,
                    ))
                },
            )
            .map_err(StoreError::from)
    }
}

fn truncate_error(error: &str) -> String {
    error.chars().take(1_024).collect()
}

pub(super) fn topology_layout(
    topology: &[(String, Option<String>)],
) -> (HashMap<String, u32>, HashSet<String>) {
    let parents = topology
        .iter()
        .map(|(id, parent)| (id.as_str(), parent.as_deref()))
        .collect::<HashMap<_, _>>();
    let parents_with_children = topology
        .iter()
        .filter_map(|(_, parent)| parent.clone())
        .collect::<HashSet<_>>();

    // Resolve each parent chain once. The previous implementation walked from every span to its
    // root independently, which made a 20k-deep agent trace quadratic and blocked the GPUI thread
    // whenever a finding loaded its evidence spans.
    let mut resolved = HashMap::<&str, u32>::with_capacity(topology.len());
    for (id, _) in topology {
        if resolved.contains_key(id.as_str()) {
            continue;
        }

        let mut path = Vec::new();
        let mut positions = HashMap::<&str, usize>::new();
        let mut current = id.as_str();
        let mut known_parent_depth = None;
        let mut cyclic = false;

        loop {
            if let Some(depth) = resolved.get(current).copied() {
                known_parent_depth = Some(depth);
                break;
            }
            if positions.insert(current, path.len()).is_some() {
                cyclic = true;
                break;
            }

            let Some(parent) = parents.get(current) else {
                // A missing parent is an orphan root until its parent arrives.
                break;
            };
            path.push(current);
            let Some(parent) = parent else {
                break;
            };
            current = parent;
        }

        if cyclic {
            // Cycles are invalid topology. Keep every affected row visible at root depth rather
            // than allowing an unbounded walk or inventing a misleading hierarchy.
            for node in path {
                resolved.insert(node, 0);
            }
            continue;
        }

        let mut next_depth = known_parent_depth.map(|depth| depth.saturating_add(1));
        for node in path.into_iter().rev() {
            let depth = next_depth.unwrap_or(0);
            resolved.insert(node, depth);
            next_depth = Some(depth.saturating_add(1));
        }
    }

    let depths = topology
        .iter()
        .map(|(id, _)| (id.clone(), resolved.get(id.as_str()).copied().unwrap_or(0)))
        .collect();
    (depths, parents_with_children)
}

fn topology_projection(
    topology: &[(String, Option<String>)],
) -> (Vec<String>, HashMap<String, u32>, HashSet<String>) {
    let (depths, _) = topology_layout(topology);
    let input_order = topology
        .iter()
        .enumerate()
        .map(|(index, (span_id, _))| (span_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut children = HashMap::<String, Vec<String>>::new();
    let mut roots = Vec::new();
    let mut parents_with_children = HashSet::new();

    for (span_id, parent_span_id) in topology {
        let valid_parent = parent_span_id.as_ref().filter(|parent_span_id| {
            depths.get(*parent_span_id).is_some_and(|parent_depth| {
                depths.get(span_id).copied() == Some(parent_depth.saturating_add(1))
            })
        });
        if let Some(parent_span_id) = valid_parent {
            children
                .entry(parent_span_id.clone())
                .or_default()
                .push(span_id.clone());
            parents_with_children.insert(parent_span_id.clone());
        } else {
            roots.push(span_id.clone());
        }
    }
    roots.sort_by_key(|span_id| {
        input_order
            .get(span_id.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });
    for child_ids in children.values_mut() {
        child_ids.sort_by_key(|span_id| {
            input_order
                .get(span_id.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
    }

    let mut ordered = Vec::with_capacity(topology.len());
    let mut visited = HashSet::with_capacity(topology.len());
    let mut stack = roots.into_iter().rev().collect::<Vec<_>>();
    while let Some(span_id) = stack.pop() {
        if !visited.insert(span_id.clone()) {
            continue;
        }
        ordered.push(span_id.clone());
        if let Some(child_ids) = children.get(&span_id) {
            stack.extend(child_ids.iter().rev().cloned());
        }
    }
    for (span_id, _) in topology {
        if visited.insert(span_id.clone()) {
            ordered.push(span_id.clone());
        }
    }
    (ordered, depths, parents_with_children)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalized_projection_is_depth_first_even_when_timestamps_interleave_subtrees() {
        let topology = vec![
            ("root".into(), None),
            ("child-a".into(), Some("root".into())),
            ("child-b".into(), Some("root".into())),
            ("grandchild-a".into(), Some("child-a".into())),
        ];

        let (order, depths, parents) = topology_projection(&topology);

        assert_eq!(order, ["root", "child-a", "grandchild-a", "child-b"]);
        assert_eq!(depths["grandchild-a"], 2);
        assert!(parents.contains("root"));
        assert!(parents.contains("child-a"));
    }

    #[test]
    fn cycles_remain_visible_as_roots_without_inventing_children() {
        let topology = vec![
            ("a".into(), Some("b".into())),
            ("b".into(), Some("a".into())),
        ];

        let (order, depths, parents) = topology_projection(&topology);

        assert_eq!(order, ["a", "b"]);
        assert_eq!(depths["a"], 0);
        assert_eq!(depths["b"], 0);
        assert!(parents.is_empty());
    }

    #[test]
    fn multiple_roots_and_missing_parents_remain_visible_in_stable_order() {
        let topology = vec![
            ("root-a".into(), None),
            ("child-a".into(), Some("root-a".into())),
            ("root-b".into(), None),
            ("orphan".into(), Some("missing-parent".into())),
        ];

        let (order, depths, parents) = topology_projection(&topology);

        assert_eq!(order, ["root-a", "child-a", "root-b", "orphan"]);
        assert_eq!(depths["root-a"], 0);
        assert_eq!(depths["child-a"], 1);
        assert_eq!(depths["root-b"], 0);
        assert_eq!(depths["orphan"], 0);
        assert_eq!(parents, HashSet::from(["root-a".to_string()]));
    }

    #[test]
    fn deep_twenty_thousand_span_projection_is_iterative_and_linear() {
        let topology = (0..20_000)
            .map(|index| {
                (
                    format!("span-{index:05}"),
                    (index > 0).then(|| format!("span-{:05}", index - 1)),
                )
            })
            .collect::<Vec<_>>();

        let (order, depths, parents) = topology_projection(&topology);

        assert_eq!(order.len(), 20_000);
        assert_eq!(order.first().map(String::as_str), Some("span-00000"));
        assert_eq!(order.last().map(String::as_str), Some("span-19999"));
        assert_eq!(depths["span-19999"], 19_999);
        assert_eq!(parents.len(), 19_999);
    }
}
