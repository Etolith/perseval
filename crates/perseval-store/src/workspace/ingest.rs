use super::*;

#[derive(Debug, Clone)]
struct CurrentSpanVersion {
    version: i64,
    content_hash: String,
    status_code: i32,
    start_time_unix_nano: i64,
    end_time_unix_nano: i64,
}

type CurrentSpanVersionsByTrace = HashMap<(String, u64), HashMap<String, CurrentSpanVersion>>;

struct ProjectedPayloadBlob {
    sha256: String,
    original_bytes: u64,
    compressed: Vec<u8>,
}

struct PendingSpanProjection {
    span_index: usize,
    revision: u64,
    version: i64,
    is_current: bool,
    duration_nano: u64,
    attributes_json: String,
    payload_refs_json: String,
    resource_json: String,
    scope_json: String,
    payload_identities_json: String,
}

#[derive(Debug, Clone)]
struct IncrementalTraceSummary {
    span_count: i64,
    error_count: i64,
    start_time_unix_nano: i64,
    end_time_unix_nano: i64,
    start_boundary_dirty: bool,
    end_boundary_dirty: bool,
}

impl IncrementalTraceSummary {
    fn apply_new(&mut self, span: &SpanUpsertV1) {
        let start = span.start_time_unix_nano as i64;
        let end = span.end_time_unix_nano as i64;
        if self.span_count == 0 {
            self.start_time_unix_nano = start;
            self.end_time_unix_nano = end;
        } else {
            self.start_time_unix_nano = self.start_time_unix_nano.min(start);
            self.end_time_unix_nano = self.end_time_unix_nano.max(end);
        }
        self.span_count = self.span_count.saturating_add(1);
        if span.status_code == 2 {
            self.error_count = self.error_count.saturating_add(1);
        }
    }

    fn apply_correction(&mut self, previous: &CurrentSpanVersion, span: &SpanUpsertV1) {
        self.error_count = self
            .error_count
            .saturating_add(i64::from(span.status_code == 2))
            .saturating_sub(i64::from(previous.status_code == 2));
        let start = span.start_time_unix_nano as i64;
        let end = span.end_time_unix_nano as i64;
        if previous.start_time_unix_nano == self.start_time_unix_nano
            && start > previous.start_time_unix_nano
        {
            self.start_boundary_dirty = true;
        }
        if previous.end_time_unix_nano == self.end_time_unix_nano
            && end < previous.end_time_unix_nano
        {
            self.end_boundary_dirty = true;
        }
        self.start_time_unix_nano = self.start_time_unix_nano.min(start);
        self.end_time_unix_nano = self.end_time_unix_nano.max(end);
    }
}

impl WorkspaceStore {
    pub fn journal_batch(
        &self,
        batch: &mut SpanUpsertBatchV1,
        raw_wire_payload: &[u8],
        wire_encoding: &str,
        _inline_attribute_bytes: usize,
    ) -> Result<JournalReceipt, StoreError> {
        let mut stage_samples = Vec::with_capacity(4);

        let build_started = Instant::now();
        for span in &mut batch.spans {
            span.content_hash.clear();
            span.content_hash = hex::encode(Sha256::digest(serde_json::to_vec(span)?));
        }
        let normalized = serde_json::to_vec(batch)?;
        let mut build =
            PipelineStageSampleV1::new(PipelineStageV1::JournalBuild, elapsed_nano(build_started));
        build.item_count = batch.spans.len() as u64;
        build.byte_count = normalized.len() as u64;
        stage_samples.push(build);

        let raw_started = Instant::now();
        let raw_blob = self.blobs.put(raw_wire_payload)?;
        let mut raw = PipelineStageSampleV1::new(
            PipelineStageV1::RawBlobDurability,
            elapsed_nano(raw_started),
        );
        raw.byte_count = raw_wire_payload.len() as u64;
        stage_samples.push(raw);

        let normalized_started = Instant::now();
        let normalized_blob = self.blobs.put(&normalized)?;
        let mut normalized_sample = PipelineStageSampleV1::new(
            PipelineStageV1::NormalizedBlobDurability,
            elapsed_nano(normalized_started),
        );
        normalized_sample.byte_count = normalized.len() as u64;
        stage_samples.push(normalized_sample);

        let commit_started = Instant::now();
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let existing = transaction
            .query_row(
                "SELECT sequence FROM ingest_journal WHERE workspace_id = ?1 AND source_id = ?2 AND raw_blob_hash = ?3",
                params![self.workspace_id, batch.source_id, raw_blob.sha256],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(sequence) = existing {
            stage_samples.push(PipelineStageSampleV1::new(
                PipelineStageV1::JournalCommit,
                elapsed_nano(commit_started),
            ));
            return Ok(JournalReceipt {
                journal_sequence: sequence as u64,
                duplicate_request: true,
                raw_blob,
                normalized_blob,
                stage_samples,
            });
        }
        transaction.execute(
            "INSERT INTO ingest_journal (
                workspace_id, source_id, raw_blob_hash, normalized_blob_hash,
                wire_encoding, received_at_unix_ms, accepted_spans, rejected_spans, projected,
                normalized_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)",
            params![
                self.workspace_id,
                batch.source_id,
                raw_blob.sha256,
                normalized_blob.sha256,
                wire_encoding,
                now,
                batch.spans.len() as i64,
                batch.rejected_spans as i64,
                normalized.len() as i64,
            ],
        )?;
        let sequence = transaction.last_insert_rowid() as u64;
        transaction.execute(
            "INSERT INTO source_health(workspace_id, source_id, accepted_spans, rejected_spans)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(workspace_id, source_id) DO UPDATE SET
                accepted_spans = accepted_spans + excluded.accepted_spans,
                rejected_spans = rejected_spans + excluded.rejected_spans",
            params![
                self.workspace_id,
                batch.source_id,
                batch.spans.len() as i64,
                batch.rejected_spans as i64,
            ],
        )?;
        transaction.commit()?;
        stage_samples.push(PipelineStageSampleV1::new(
            PipelineStageV1::JournalCommit,
            elapsed_nano(commit_started),
        ));
        Ok(JournalReceipt {
            journal_sequence: sequence,
            duplicate_request: false,
            raw_blob,
            normalized_blob,
            stage_samples,
        })
    }

    pub fn project_journal(&self, sequence: u64) -> Result<Vec<TraceDeltaV1>, StoreError> {
        self.project_journals_with_inline_limit(
            &[sequence],
            crate::model::DEFAULT_INLINE_ATTRIBUTE_BYTES,
        )
    }

    pub fn project_journals(&self, sequences: &[u64]) -> Result<Vec<TraceDeltaV1>, StoreError> {
        self.project_journals_with_inline_limit(
            sequences,
            crate::model::DEFAULT_INLINE_ATTRIBUTE_BYTES,
        )
    }

    pub fn project_journals_with_inline_limit(
        &self,
        sequences: &[u64],
        inline_attribute_bytes: usize,
    ) -> Result<Vec<TraceDeltaV1>, StoreError> {
        let deserialize_started = Instant::now();
        let mut active_sequences = Vec::new();
        let mut merged: Option<SpanUpsertBatchV1> = None;
        let mut normalized_bytes = 0_u64;
        for sequence in sequences.iter().copied().collect::<BTreeSet<_>>() {
            let normalized_hash = {
                let control = self.control.lock().expect("control store lock poisoned");
                let row = control
                    .query_row(
                        "SELECT normalized_blob_hash, projected FROM ingest_journal WHERE sequence = ?1",
                        params![sequence as i64],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .optional()?;
                let Some((hash, projected)) = row else {
                    return Err(StoreError::Invalid(format!(
                        "missing journal sequence {sequence}"
                    )));
                };
                if projected != 0 {
                    continue;
                }
                hash
            };
            let bytes = self.blobs.get(&normalized_hash, usize::MAX)?;
            normalized_bytes = normalized_bytes.saturating_add(bytes.len() as u64);
            let mut batch: SpanUpsertBatchV1 = serde_json::from_slice(&bytes)?;
            active_sequences.push(sequence);
            if let Some(merged) = &mut merged {
                merged.spans.append(&mut batch.spans);
                merged.rejected_spans = merged.rejected_spans.saturating_add(batch.rejected_spans);
            } else {
                merged = Some(batch);
            }
        }
        let Some(mut batch) = merged else {
            return Ok(Vec::new());
        };
        let mut deserialize = PipelineStageSampleV1::new(
            PipelineStageV1::ProjectionDeserialization,
            elapsed_nano(deserialize_started),
        );
        deserialize.item_count = active_sequences.len() as u64;
        deserialize.byte_count = normalized_bytes;
        deserialize.rows_deserialized = batch.spans.len() as u64;

        let payload_started = Instant::now();
        let payload_blobs =
            Self::externalize_payloads_for_projection(&mut batch, inline_attribute_bytes)?;
        let mut payload = PipelineStageSampleV1::new(
            PipelineStageV1::PayloadBlobDurability,
            elapsed_nano(payload_started),
        );
        payload.item_count = batch
            .spans
            .iter()
            .map(|span| span.payload_refs.len() as u64)
            .sum();
        payload.byte_count = batch
            .spans
            .iter()
            .flat_map(|span| span.payload_refs.values())
            .map(|blob| blob.original_bytes)
            .sum();

        let projection_started = Instant::now();
        let deltas = self.project_batch(&active_sequences, &batch, &payload_blobs)?;
        let mut projection = PipelineStageSampleV1::new(
            PipelineStageV1::Projection,
            elapsed_nano(projection_started),
        );
        projection.item_count = batch.spans.len() as u64;
        projection.rows_scanned = batch.spans.len() as u64;
        let _ = self.record_pipeline_stages(&[deserialize, payload, projection]);
        Ok(deltas)
    }

    pub fn pending_journal_sequences(&self, limit: usize) -> Result<Vec<u64>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT sequence FROM ingest_journal WHERE projected = 0 ORDER BY sequence LIMIT ?1",
        )?;
        statement
            .query_map(params![limit as i64], |row| row.get::<_, i64>(0))?
            .map(|value| Ok(value? as u64))
            .collect()
    }

    pub fn pending_projection_sequences(
        &self,
        maximum_rows: usize,
        maximum_spans: usize,
        maximum_bytes: usize,
    ) -> Result<Vec<u64>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT sequence, accepted_spans, normalized_bytes
             FROM ingest_journal WHERE projected = 0 ORDER BY sequence LIMIT ?1",
        )?;
        let rows = statement
            .query_map(params![maximum_rows as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, i64>(1)?.max(0) as usize,
                    row.get::<_, i64>(2)?.max(0) as usize,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut sequences = Vec::new();
        let mut spans = 0usize;
        let mut bytes = 0usize;
        for (sequence, row_spans, row_bytes) in rows {
            if !sequences.is_empty()
                && (spans.saturating_add(row_spans) > maximum_spans
                    || bytes.saturating_add(row_bytes) > maximum_bytes)
            {
                break;
            }
            sequences.push(sequence);
            spans = spans.saturating_add(row_spans);
            bytes = bytes.saturating_add(row_bytes);
        }
        Ok(sequences)
    }

    fn externalize_payloads_for_projection(
        batch: &mut SpanUpsertBatchV1,
        inline_attribute_bytes: usize,
    ) -> Result<Vec<ProjectedPayloadBlob>, StoreError> {
        let mut blobs = BTreeMap::new();
        for span in &mut batch.spans {
            let keys = span.attributes.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                let value = &span.attributes[&key];
                let encoded = serde_json::to_vec(value)?;
                if is_payload_key(&key) || encoded.len() > inline_attribute_bytes {
                    let fingerprint = payload_fingerprint(value)?;
                    let sha256 = hex::encode(Sha256::digest(&encoded));
                    let blob = BlobRefV1 {
                        sha256: sha256.clone(),
                        original_bytes: encoded.len() as u64,
                    };
                    if !blobs.contains_key(&sha256) {
                        blobs.insert(
                            sha256.clone(),
                            ProjectedPayloadBlob {
                                sha256,
                                original_bytes: encoded.len() as u64,
                                compressed: zstd::stream::encode_all(encoded.as_slice(), 3)?,
                            },
                        );
                    }
                    span.attributes.remove(&key);
                    span.payload_refs.insert(key.clone(), blob.clone());
                    span.payload_identities.insert(
                        key,
                        PayloadIdentityV1 {
                            schema_version: PAYLOAD_IDENTITY_SCHEMA_VERSION.into(),
                            fingerprint,
                            blob: Some(blob.clone()),
                            original_bytes: blob.original_bytes,
                            quality: PayloadIdentityQualityV1::Explicit,
                        },
                    );
                }
            }
        }
        Ok(blobs.into_values().collect())
    }

    fn project_batch(
        &self,
        journal_sequences: &[u64],
        batch: &SpanUpsertBatchV1,
        payload_blobs: &[ProjectedPayloadBlob],
    ) -> Result<Vec<TraceDeltaV1>, StoreError> {
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let analytics = self
            .analytics
            .lock()
            .expect("analytics store lock poisoned");
        let transaction = control.transaction()?;
        let mut touched: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut trace_revisions = HashMap::new();
        let mut summaries = HashMap::new();
        let applied_sequences =
            load_applied_journal_sequences(&analytics, &self.workspace_id, journal_sequences)?;
        let requires_control_reconciliation = !applied_sequences.is_empty();

        analytics.execute_batch("BEGIN TRANSACTION")?;
        let projection_result = (|| -> Result<(), StoreError> {
            if !payload_blobs.is_empty() {
                analytics.execute_batch(
                    "CREATE TEMP TABLE IF NOT EXISTS incoming_payload_blobs(
                        sha256 VARCHAR, original_bytes BIGINT, compressed BLOB
                     );
                     DELETE FROM incoming_payload_blobs;",
                )?;
                {
                    let mut appender = analytics.appender("incoming_payload_blobs")?;
                    for payload in payload_blobs {
                        appender.append_row(duck_params![
                            payload.sha256,
                            payload.original_bytes as i64,
                            payload.compressed
                        ])?;
                    }
                    appender.flush()?;
                }
                analytics.execute_batch(
                    "INSERT INTO payload_blobs
                     SELECT sha256, original_bytes, compressed FROM incoming_payload_blobs
                     ON CONFLICT(sha256) DO NOTHING",
                )?;
            }
            for span in &batch.spans {
                let (revision, reopened) =
                    ensure_logical_trace(&transaction, &self.workspace_id, span, now)?;
                trace_revisions
                    .entry(span.logical_trace_id.clone())
                    .and_modify(|current: &mut (u64, bool)| {
                        debug_assert_eq!(current.0, revision);
                        current.1 |= reopened;
                    })
                    .or_insert((revision, reopened));
            }

            let mut current_versions =
                load_incoming_current_versions(&analytics, &trace_revisions, &batch.spans)?;
            summaries = load_incremental_summaries(&transaction, &trace_revisions)?;
            let mut pending = Vec::<PendingSpanProjection>::new();
            let mut latest_pending = HashMap::new();
            for (span_index, span) in batch.spans.iter().enumerate() {
                let (revision, _) = trace_revisions[&span.logical_trace_id];
                let key = (span.logical_trace_id.clone(), revision);
                let current = current_versions
                    .get(&key)
                    .and_then(|versions| versions.get(&span.external_span_id))
                    .cloned();
                if current
                    .as_ref()
                    .is_some_and(|stored| stored.content_hash == span.content_hash)
                {
                    continue;
                }
                let version = current
                    .as_ref()
                    .map(|stored| stored.version + 1)
                    .unwrap_or(1);
                let summary = summaries
                    .get_mut(&key)
                    .expect("every projected trace has a control summary");
                if let Some(previous) = &current {
                    summary.apply_correction(previous, span);
                } else {
                    summary.apply_new(span);
                }
                let duration = span
                    .end_time_unix_nano
                    .saturating_sub(span.start_time_unix_nano);
                let pending_identity = (
                    span.logical_trace_id.clone(),
                    revision,
                    span.external_span_id.clone(),
                );
                if let Some(previous_index) = latest_pending.insert(pending_identity, pending.len())
                {
                    pending[previous_index].is_current = false;
                }
                pending.push(PendingSpanProjection {
                    span_index,
                    revision,
                    version,
                    is_current: true,
                    duration_nano: duration,
                    attributes_json: serde_json::to_string(&span.attributes)?,
                    payload_refs_json: serde_json::to_string(&span.payload_refs)?,
                    resource_json: serde_json::to_string(&span.resource)?,
                    scope_json: serde_json::to_string(&span.scope)?,
                    payload_identities_json: serde_json::to_string(&span.payload_identities)?,
                });
                current_versions
                    .entry(key)
                    .or_insert_with(HashMap::new)
                    .insert(
                        span.external_span_id.clone(),
                        CurrentSpanVersion {
                            version,
                            content_hash: span.content_hash.clone(),
                            status_code: span.status_code,
                            start_time_unix_nano: span.start_time_unix_nano as i64,
                            end_time_unix_nano: span.end_time_unix_nano as i64,
                        },
                    );
                touched
                    .entry(span.logical_trace_id.clone())
                    .or_default()
                    .insert(span.external_span_id.clone());
            }
            append_span_projections(&analytics, batch, &pending)?;
            for journal_sequence in journal_sequences {
                analytics.execute(
                    "INSERT INTO projected_journal_sequences(
                        workspace_id, journal_sequence, projected_at_unix_ms
                     ) VALUES (?1, ?2, ?3)
                     ON CONFLICT(workspace_id, journal_sequence) DO NOTHING",
                    duck_params![self.workspace_id, *journal_sequence as i64, now],
                )?;
            }
            Ok(())
        })();
        if let Err(error) = projection_result {
            let _ = analytics.execute_batch("ROLLBACK");
            return Err(error);
        }
        analytics.execute_batch("COMMIT")?;

        if requires_control_reconciliation {
            for span in &batch.spans {
                touched
                    .entry(span.logical_trace_id.clone())
                    .or_default()
                    .insert(span.external_span_id.clone());
            }
        }
        self.update_live_topology_indexes(&trace_revisions, &batch.spans, &touched);

        let mut deltas = Vec::new();
        for (trace_id, changed) in touched {
            let (revision, reopened) = trace_revisions[&trace_id];
            let summary_delta = if requires_control_reconciliation {
                load_full_trace_summary(&analytics, &trace_id, revision)?
            } else {
                summaries
                    .remove(&(trace_id.clone(), revision))
                    .expect("touched trace has an incremental summary")
            };
            persist_incremental_summary(
                &transaction,
                &analytics,
                &trace_id,
                revision,
                now,
                summary_delta,
            )?;
            let summary = query_run_transaction(&transaction, &self.workspace_id, &trace_id)?
                .ok_or_else(|| StoreError::Invalid("projected trace disappeared".into()))?;
            deltas.push(insert_delta_transaction(
                &transaction,
                &self.workspace_id,
                summary,
                if reopened {
                    TraceChangeKind::Reopened
                } else {
                    TraceChangeKind::Upserted
                },
                changed.into_iter().collect(),
            )?);
        }
        for journal_sequence in journal_sequences {
            transaction.execute(
                "UPDATE ingest_journal SET projected = 1, projected_at_unix_ms = ?1 WHERE sequence = ?2",
                params![now, *journal_sequence as i64],
            )?;
        }
        let checkpoint = journal_sequences.iter().copied().max().unwrap_or_default();
        transaction.execute(
            "INSERT INTO projector_checkpoint (workspace_id, journal_sequence)
             VALUES (?1, ?2)
             ON CONFLICT(workspace_id) DO UPDATE SET journal_sequence = MAX(journal_sequence, excluded.journal_sequence)",
            params![self.workspace_id, checkpoint as i64],
        )?;
        transaction.commit()?;
        Ok(deltas)
    }
}

fn load_incoming_current_versions(
    analytics: &DuckConnection,
    trace_revisions: &HashMap<String, (u64, bool)>,
    spans: &[SpanUpsertV1],
) -> Result<CurrentSpanVersionsByTrace, StoreError> {
    const IDENTITY_QUERY_CHUNK: usize = 500;
    let mut incoming = HashMap::<String, BTreeSet<String>>::new();
    for span in spans {
        incoming
            .entry(span.logical_trace_id.clone())
            .or_default()
            .insert(span.external_span_id.clone());
    }
    let mut result = HashMap::new();
    for (trace_id, span_ids) in incoming {
        let revision = trace_revisions[&trace_id].0;
        let versions = result
            .entry((trace_id.clone(), revision))
            .or_insert_with(HashMap::new);
        let span_ids = span_ids.into_iter().collect::<Vec<_>>();
        for chunk in span_ids.chunks(IDENTITY_QUERY_CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT span_id, span_version, content_hash, status_code,
                        start_time_unix_nano, end_time_unix_nano
                 FROM spans
                 WHERE logical_trace_id = ? AND revision = ? AND is_current = TRUE
                   AND span_id IN ({placeholders})"
            );
            let revision_value = revision as i64;
            let mut parameters = Vec::<&dyn duckdb::ToSql>::with_capacity(chunk.len() + 2);
            parameters.push(&trace_id);
            parameters.push(&revision_value);
            parameters.extend(chunk.iter().map(|span_id| span_id as &dyn duckdb::ToSql));
            let mut statement = analytics.prepare(&sql)?;
            let rows = statement.query_map(duckdb::params_from_iter(parameters), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    CurrentSpanVersion {
                        version: row.get(1)?,
                        content_hash: row.get(2)?,
                        status_code: row.get(3)?,
                        start_time_unix_nano: row.get(4)?,
                        end_time_unix_nano: row.get(5)?,
                    },
                ))
            })?;
            for row in rows {
                let (span_id, version) = row?;
                versions.insert(span_id, version);
            }
        }
    }
    Ok(result)
}

fn load_applied_journal_sequences(
    analytics: &DuckConnection,
    workspace_id: &str,
    journal_sequences: &[u64],
) -> Result<BTreeSet<u64>, StoreError> {
    let mut applied = BTreeSet::new();
    for chunk in journal_sequences.chunks(500) {
        if chunk.is_empty() {
            continue;
        }
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT journal_sequence FROM projected_journal_sequences
             WHERE workspace_id = ? AND journal_sequence IN ({placeholders})"
        );
        let mut values = Vec::<Box<dyn duckdb::ToSql>>::with_capacity(chunk.len() + 1);
        values.push(Box::new(workspace_id.to_owned()));
        values.extend(
            chunk
                .iter()
                .map(|sequence| Box::new(*sequence as i64) as Box<dyn duckdb::ToSql>),
        );
        let references = values
            .iter()
            .map(|value| value.as_ref() as &dyn duckdb::ToSql)
            .collect::<Vec<_>>();
        let mut statement = analytics.prepare(&sql)?;
        let rows = statement.query_map(references.as_slice(), |row| row.get::<_, i64>(0))?;
        for sequence in rows {
            applied.insert(sequence? as u64);
        }
    }
    Ok(applied)
}

fn load_full_trace_summary(
    analytics: &DuckConnection,
    trace_id: &str,
    revision: u64,
) -> Result<IncrementalTraceSummary, StoreError> {
    analytics
        .query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN status_code = 2 THEN 1 ELSE 0 END), 0),
                    COALESCE(MIN(start_time_unix_nano), 0),
                    COALESCE(MAX(end_time_unix_nano), 0)
             FROM spans
             WHERE logical_trace_id = ?1 AND revision = ?2 AND is_current = TRUE",
            duck_params![trace_id, revision as i64],
            |row| {
                Ok(IncrementalTraceSummary {
                    span_count: row.get(0)?,
                    error_count: row.get(1)?,
                    start_time_unix_nano: row.get(2)?,
                    end_time_unix_nano: row.get(3)?,
                    start_boundary_dirty: false,
                    end_boundary_dirty: false,
                })
            },
        )
        .map_err(StoreError::from)
}

fn append_span_projections(
    analytics: &DuckConnection,
    batch: &SpanUpsertBatchV1,
    pending: &[PendingSpanProjection],
) -> Result<(), StoreError> {
    if pending.is_empty() {
        return Ok(());
    }
    analytics.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS incoming_current_span_keys(
            logical_trace_id VARCHAR, revision BIGINT, span_id VARCHAR
         );
         DELETE FROM incoming_current_span_keys;",
    )?;
    let identities = pending
        .iter()
        .map(|projection| {
            let span = &batch.spans[projection.span_index];
            (
                span.logical_trace_id.as_str(),
                projection.revision,
                span.external_span_id.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    {
        let mut appender = analytics.appender("incoming_current_span_keys")?;
        for (trace_id, revision, span_id) in identities {
            appender.append_row(duck_params![trace_id, revision as i64, span_id])?;
        }
        appender.flush()?;
    }
    analytics.execute_batch(
        "UPDATE spans AS stored SET is_current = FALSE
         FROM incoming_current_span_keys AS incoming
         WHERE stored.logical_trace_id = incoming.logical_trace_id
           AND stored.revision = incoming.revision
           AND stored.span_id = incoming.span_id
           AND stored.is_current = TRUE",
    )?;

    let span_columns = [
        "logical_trace_id",
        "revision",
        "span_id",
        "span_version",
        "is_current",
        "parent_span_id",
        "name",
        "category",
        "start_time_unix_nano",
        "end_time_unix_nano",
        "duration_nano",
        "status_code",
        "status_message",
        "content_hash",
        "attributes_json",
        "payload_refs_json",
        "resource_json",
        "scope_json",
        "topology_order",
        "topology_depth",
        "topology_has_children",
        "topology_projection_version",
        "payload_identities_json",
        "source_id",
        "decoder_version",
        "semantic_mapping_version",
    ];
    {
        let mut appender = analytics.appender_with_columns("spans", &span_columns)?;
        for projection in pending {
            let span = &batch.spans[projection.span_index];
            appender.append_row(duck_params![
                span.logical_trace_id,
                projection.revision as i64,
                span.external_span_id,
                projection.version,
                projection.is_current,
                span.external_parent_span_id,
                span.name,
                span.category,
                span.start_time_unix_nano as i64,
                span.end_time_unix_nano as i64,
                projection.duration_nano as i64,
                span.status_code,
                span.status_message,
                span.content_hash,
                projection.attributes_json,
                projection.payload_refs_json,
                projection.resource_json,
                projection.scope_json,
                Option::<i64>::None,
                Option::<i64>::None,
                Option::<bool>::None,
                Option::<i64>::None,
                projection.payload_identities_json,
                span.source_id,
                span.decoder_version,
                span.semantic_mapping_version,
            ])?;
        }
        appender.flush()?;
    }

    let event_columns = [
        "logical_trace_id",
        "revision",
        "span_id",
        "span_version",
        "event_index",
        "name",
        "timestamp_unix_nano",
        "attributes_json",
        "evidence_identity",
        "dropped_attributes_count",
    ];
    {
        let mut appender = analytics.appender_with_columns("span_events", &event_columns)?;
        for projection in pending {
            let span = &batch.spans[projection.span_index];
            for (index, event) in span.events.iter().enumerate() {
                let attributes = serde_json::to_string(&event.attributes)?;
                let identity = event_evidence_identity(
                    &span.logical_trace_id,
                    projection.revision,
                    &span.external_span_id,
                    projection.version,
                    index,
                );
                appender.append_row(duck_params![
                    span.logical_trace_id,
                    projection.revision as i64,
                    span.external_span_id,
                    projection.version,
                    index as i64,
                    event.name,
                    event.timestamp_unix_nano as i64,
                    attributes,
                    identity,
                    event.dropped_attributes_count as i64,
                ])?;
            }
        }
        appender.flush()?;
    }

    let link_columns = [
        "logical_trace_id",
        "revision",
        "span_id",
        "span_version",
        "link_index",
        "linked_trace_id",
        "linked_span_id",
        "attributes_json",
        "evidence_identity",
        "trace_state",
        "dropped_attributes_count",
        "flags",
    ];
    {
        let mut appender = analytics.appender_with_columns("span_links", &link_columns)?;
        for projection in pending {
            let span = &batch.spans[projection.span_index];
            for (index, link) in span.links.iter().enumerate() {
                let attributes = serde_json::to_string(&link.attributes)?;
                let identity = link_evidence_identity(
                    &span.logical_trace_id,
                    projection.revision,
                    &span.external_span_id,
                    projection.version,
                    index,
                );
                appender.append_row(duck_params![
                    span.logical_trace_id,
                    projection.revision as i64,
                    span.external_span_id,
                    projection.version,
                    index as i64,
                    link.trace_id,
                    link.span_id,
                    attributes,
                    identity,
                    link.trace_state,
                    link.dropped_attributes_count as i64,
                    link.flags as i64,
                ])?;
            }
        }
        appender.flush()?;
    }
    Ok(())
}

fn load_incremental_summaries(
    transaction: &rusqlite::Transaction<'_>,
    trace_revisions: &HashMap<String, (u64, bool)>,
) -> Result<HashMap<(String, u64), IncrementalTraceSummary>, StoreError> {
    trace_revisions
        .iter()
        .map(|(trace_id, (revision, _))| {
            let summary = transaction.query_row(
                "SELECT span_count, error_count, start_time_unix_nano, end_time_unix_nano
                 FROM logical_traces WHERE logical_trace_id = ?1",
                params![trace_id],
                |row| {
                    Ok(IncrementalTraceSummary {
                        span_count: row.get(0)?,
                        error_count: row.get(1)?,
                        start_time_unix_nano: row.get(2)?,
                        end_time_unix_nano: row.get(3)?,
                        start_boundary_dirty: false,
                        end_boundary_dirty: false,
                    })
                },
            )?;
            Ok(((trace_id.clone(), *revision), summary))
        })
        .collect()
}

fn persist_incremental_summary(
    transaction: &rusqlite::Transaction<'_>,
    analytics: &DuckConnection,
    trace_id: &str,
    revision: u64,
    now: i64,
    mut summary: IncrementalTraceSummary,
) -> Result<(), StoreError> {
    if summary.start_boundary_dirty || summary.end_boundary_dirty {
        let (start, end): (i64, i64) = analytics.query_row(
            "SELECT COALESCE(MIN(start_time_unix_nano), 0),
                    COALESCE(MAX(end_time_unix_nano), 0)
             FROM spans WHERE logical_trace_id = ?1 AND revision = ?2 AND is_current = TRUE",
            duck_params![trace_id, revision as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        summary.start_time_unix_nano = start;
        summary.end_time_unix_nano = end;
    }
    transaction.execute(
        "UPDATE logical_traces SET span_count = ?1, error_count = ?2,
                start_time_unix_nano = ?3, end_time_unix_nano = ?4,
                last_committed_unix_ms = ?5 WHERE logical_trace_id = ?6",
        params![
            summary.span_count,
            summary.error_count.max(0),
            summary.start_time_unix_nano,
            summary.end_time_unix_nano,
            now,
            trace_id
        ],
    )?;
    Ok(())
}
