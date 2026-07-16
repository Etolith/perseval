use super::*;

pub(super) fn writer_loop(
    store: Arc<WorkspaceStore>,
    hub: Arc<DeltaHub>,
    shared: Arc<WriterShared>,
    config: PersevalConfigV1,
    receiver: mpsc::Receiver<WriterCommand>,
    candidate_job_sender: mpsc::SyncSender<String>,
) {
    let mut projection_retry = ProjectionRetryState::new(&config);
    let mut last_lifecycle = Instant::now();
    let mut last_metrics_flush = Instant::now();
    'writer: loop {
        match receiver.recv_timeout(Duration::from_millis(config.stream.microbatch_wait_ms)) {
            Ok(WriterCommand::Ingest {
                submission,
                admitted_bytes,
                response,
            }) => {
                let mut sequences = Vec::new();
                let mut batch_spans = 0usize;
                let mut batch_bytes = 0usize;
                if let Some((sequence, spans, bytes)) = journal_ingest(
                    &store,
                    &shared,
                    &config,
                    submission,
                    admitted_bytes,
                    response,
                ) {
                    sequences.push(sequence);
                    batch_spans = spans;
                    batch_bytes = bytes;
                }
                let deadline = Instant::now()
                    .checked_add(Duration::from_millis(config.stream.microbatch_wait_ms))
                    .unwrap_or_else(Instant::now);
                let mut shutdown = None;
                let mut disconnected = false;
                while batch_spans < config.stream.microbatch_spans
                    && batch_bytes < config.stream.microbatch_bytes
                {
                    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    match receiver.recv_timeout(remaining) {
                        Ok(WriterCommand::Ingest {
                            submission,
                            admitted_bytes,
                            response,
                        }) => {
                            if let Some((sequence, spans, bytes)) = journal_ingest(
                                &store,
                                &shared,
                                &config,
                                submission,
                                admitted_bytes,
                                response,
                            ) {
                                sequences.push(sequence);
                                batch_spans = batch_spans.saturating_add(spans);
                                batch_bytes = batch_bytes.saturating_add(bytes);
                            }
                        }
                        Ok(WriterCommand::Shutdown(response)) => {
                            shutdown = Some(response);
                            break;
                        }
                        Ok(WriterCommand::CreateProject { request, response }) => {
                            let _ = response.send(
                                store
                                    .create_project(&request)
                                    .map_err(|error| error.to_string()),
                            );
                        }
                        Ok(WriterCommand::PreviewEvalBatch {
                            project_id,
                            selection_spec,
                            response,
                        }) => {
                            let _ = response.send(
                                store
                                    .preview_eval_batch(&project_id, &selection_spec)
                                    .map_err(|error| error.to_string()),
                            );
                        }
                        Ok(WriterCommand::CreateEvalBatch {
                            project_id,
                            preview_id,
                            selection_hash,
                            idempotency_key,
                            response,
                        }) => {
                            let result = store
                                .queue_eval_batch(
                                    &project_id,
                                    &preview_id,
                                    &selection_hash,
                                    &idempotency_key,
                                )
                                .map_err(|error| error.to_string());
                            if let Ok(job) = &result {
                                let _ = candidate_job_sender.try_send(job.job_id.clone());
                            }
                            let _ = response.send(result);
                        }
                        Ok(WriterCommand::CancelEvalBatch { job_id, response }) => {
                            let _ = response.send(
                                store
                                    .cancel_candidate_generation_job(&job_id)
                                    .map_err(|error| error.to_string()),
                            );
                        }
                        Ok(WriterCommand::RetryEvalBatch { job_id, response }) => {
                            let result = store
                                .retry_candidate_generation_job(&job_id)
                                .map_err(|error| error.to_string());
                            if let Ok(job) = &result {
                                let _ = candidate_job_sender.try_send(job.job_id.clone());
                            }
                            let _ = response.send(result);
                        }
                        Ok(WriterCommand::ReviewEvalCandidate { request, response }) => {
                            let _ = response.send(
                                store
                                    .review_eval_candidate(&request)
                                    .map_err(|error| error.to_string()),
                            );
                        }
                        Ok(WriterCommand::SetFindingDisposition {
                            scope,
                            group_id,
                            finding_id,
                            state,
                            response,
                        }) => {
                            let _ = response.send(
                                store
                                    .set_finding_disposition(&scope, &group_id, &finding_id, state)
                                    .map_err(|error| error.to_string()),
                            );
                        }
                        Ok(WriterCommand::UndoFindingDisposition {
                            scope,
                            group_id,
                            finding_id,
                            response,
                        }) => {
                            let _ = response.send(
                                store
                                    .undo_finding_disposition(&scope, &group_id, &finding_id)
                                    .map_err(|error| error.to_string()),
                            );
                        }
                        Ok(WriterCommand::CreateEvalCandidate {
                            group_id,
                            finding_id,
                            response,
                        }) => {
                            let _ = response.send(
                                store
                                    .create_eval_candidate(&group_id, &finding_id)
                                    .map_err(|error| error.to_string()),
                            );
                        }
                        Ok(WriterCommand::ExecuteCandidateJob { job_id, response }) => {
                            let _ = response.send(
                                store
                                    .execute_candidate_generation_job(&job_id)
                                    .map(|_| ())
                                    .map_err(|error| error.to_string()),
                            );
                        }
                        Ok(WriterCommand::CommitComparison {
                            request,
                            comparison,
                            response,
                        }) => {
                            let _ = response.send(
                                store
                                    .commit_trace_comparison(&request, &comparison)
                                    .map_err(|error| error.to_string()),
                            );
                        }
                        Ok(
                            command @ (WriterCommand::EnqueueStaleAnalyses { .. }
                            | WriterCommand::MarkAnalysisStarted { .. }
                            | WriterCommand::CommitAnalysis { .. }
                            | WriterCommand::FailAnalysis { .. }
                            | WriterCommand::CommitCohort { .. }
                            | WriterCommand::AppendCohortAssignments { .. }
                            | WriterCommand::ClaimTopology { .. }
                            | WriterCommand::CommitTopologyChunk { .. }
                            | WriterCommand::FailTopology { .. }),
                        ) => {
                            handle_background_command(
                                &store,
                                &hub,
                                &shared,
                                config.stream.delta_history,
                                command,
                            );
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                if !sequences.is_empty() {
                    projection_retry
                        .schedule_after(Duration::from_millis(config.stream.microbatch_wait_ms));
                }
                if let Some(response) = shutdown {
                    let _ = response.send(());
                    break;
                }
                if disconnected {
                    break 'writer;
                }
            }
            Ok(WriterCommand::CreateProject { request, response }) => {
                let _ = response.send(
                    store
                        .create_project(&request)
                        .map_err(|error| error.to_string()),
                );
            }
            Ok(WriterCommand::PreviewEvalBatch {
                project_id,
                selection_spec,
                response,
            }) => {
                let _ = response.send(
                    store
                        .preview_eval_batch(&project_id, &selection_spec)
                        .map_err(|error| error.to_string()),
                );
            }
            Ok(WriterCommand::CreateEvalBatch {
                project_id,
                preview_id,
                selection_hash,
                idempotency_key,
                response,
            }) => {
                let result = store
                    .queue_eval_batch(&project_id, &preview_id, &selection_hash, &idempotency_key)
                    .map_err(|error| error.to_string());
                if let Ok(job) = &result {
                    let _ = candidate_job_sender.try_send(job.job_id.clone());
                }
                let _ = response.send(result);
            }
            Ok(WriterCommand::CancelEvalBatch { job_id, response }) => {
                let _ = response.send(
                    store
                        .cancel_candidate_generation_job(&job_id)
                        .map_err(|error| error.to_string()),
                );
            }
            Ok(WriterCommand::RetryEvalBatch { job_id, response }) => {
                let result = store
                    .retry_candidate_generation_job(&job_id)
                    .map_err(|error| error.to_string());
                if let Ok(job) = &result {
                    let _ = candidate_job_sender.try_send(job.job_id.clone());
                }
                let _ = response.send(result);
            }
            Ok(WriterCommand::ReviewEvalCandidate { request, response }) => {
                let _ = response.send(
                    store
                        .review_eval_candidate(&request)
                        .map_err(|error| error.to_string()),
                );
            }
            Ok(WriterCommand::SetFindingDisposition {
                scope,
                group_id,
                finding_id,
                state,
                response,
            }) => {
                let _ = response.send(
                    store
                        .set_finding_disposition(&scope, &group_id, &finding_id, state)
                        .map_err(|error| error.to_string()),
                );
            }
            Ok(WriterCommand::UndoFindingDisposition {
                scope,
                group_id,
                finding_id,
                response,
            }) => {
                let _ = response.send(
                    store
                        .undo_finding_disposition(&scope, &group_id, &finding_id)
                        .map_err(|error| error.to_string()),
                );
            }
            Ok(WriterCommand::CreateEvalCandidate {
                group_id,
                finding_id,
                response,
            }) => {
                let _ = response.send(
                    store
                        .create_eval_candidate(&group_id, &finding_id)
                        .map_err(|error| error.to_string()),
                );
            }
            Ok(WriterCommand::ExecuteCandidateJob { job_id, response }) => {
                let _ = response.send(
                    store
                        .execute_candidate_generation_job(&job_id)
                        .map(|_| ())
                        .map_err(|error| error.to_string()),
                );
            }
            Ok(WriterCommand::CommitComparison {
                request,
                comparison,
                response,
            }) => {
                let _ = response.send(
                    store
                        .commit_trace_comparison(&request, &comparison)
                        .map_err(|error| error.to_string()),
                );
            }
            Ok(
                command @ (WriterCommand::EnqueueStaleAnalyses { .. }
                | WriterCommand::MarkAnalysisStarted { .. }
                | WriterCommand::CommitAnalysis { .. }
                | WriterCommand::FailAnalysis { .. }
                | WriterCommand::CommitCohort { .. }
                | WriterCommand::AppendCohortAssignments { .. }
                | WriterCommand::ClaimTopology { .. }
                | WriterCommand::CommitTopologyChunk { .. }
                | WriterCommand::FailTopology { .. }),
            ) => {
                handle_background_command(
                    &store,
                    &hub,
                    &shared,
                    config.stream.delta_history,
                    command,
                );
            }
            Ok(WriterCommand::Shutdown(response)) => {
                let _ = response.send(());
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if last_lifecycle.elapsed() >= Duration::from_millis(config.lifecycle.sweep_ms) {
            match store.advance_lifecycle(
                now_unix_ms(),
                config.lifecycle.idle_ms,
                config.lifecycle.finalization_grace_ms,
            ) {
                Ok(deltas) => {
                    for delta in deltas {
                        hub.publish(delta);
                    }
                    let _ = store.prune_deltas(config.stream.delta_history);
                }
                Err(error) => {
                    shared
                        .health
                        .lock()
                        .expect("health lock poisoned")
                        .last_error = Some(error.to_string());
                }
            }
            last_lifecycle = Instant::now();
        }
        retry_pending_projections(&store, &hub, &shared, &config, &mut projection_retry);
        if last_metrics_flush.elapsed()
            >= Duration::from_millis(config.stream.pipeline_metrics_flush_ms)
        {
            if let Err(error) = store.flush_pipeline_stages() {
                shared
                    .health
                    .lock()
                    .expect("health lock poisoned")
                    .last_error = Some(format!("could not flush pipeline metrics: {error}"));
            }
            last_metrics_flush = Instant::now();
        }
    }
    if let Err(error) = store.flush_pipeline_stages() {
        shared
            .health
            .lock()
            .expect("health lock poisoned")
            .last_error = Some(format!("could not flush pipeline metrics: {error}"));
    }
}

struct ProjectionRetryState {
    next_attempt: Instant,
    backoff: Duration,
    initial_backoff: Duration,
    maximum_backoff: Duration,
}

impl ProjectionRetryState {
    fn new(config: &PersevalConfigV1) -> Self {
        let initial_backoff = Duration::from_millis(config.stream.projection_retry_initial_ms);
        Self {
            next_attempt: Instant::now(),
            backoff: initial_backoff,
            initial_backoff,
            maximum_backoff: Duration::from_millis(config.stream.projection_retry_max_ms),
        }
    }

    fn schedule_now(&mut self) {
        self.next_attempt = Instant::now();
    }

    fn schedule_after(&mut self, delay: Duration) {
        self.next_attempt = Instant::now()
            .checked_add(delay)
            .unwrap_or_else(Instant::now);
    }

    fn record_failure(&mut self, shared: &WriterShared, error: String) {
        let mut health = shared.health.lock().expect("health lock poisoned");
        health.projection_degraded = true;
        health.projection_retry_count = health.projection_retry_count.saturating_add(1);
        health.projection_last_error = Some(error);
        drop(health);
        self.next_attempt = Instant::now()
            .checked_add(self.backoff)
            .unwrap_or_else(Instant::now);
        self.backoff = self.backoff.saturating_mul(2).min(self.maximum_backoff);
    }

    fn record_caught_up(&mut self, shared: &WriterShared) {
        let mut health = shared.health.lock().expect("health lock poisoned");
        health.projection_degraded = false;
        health.projection_last_error = None;
        drop(health);
        self.backoff = self.initial_backoff;
        self.next_attempt = Instant::now()
            .checked_add(self.initial_backoff)
            .unwrap_or_else(Instant::now);
    }
}

fn retry_pending_projections(
    store: &WorkspaceStore,
    hub: &DeltaHub,
    shared: &WriterShared,
    config: &PersevalConfigV1,
    retry: &mut ProjectionRetryState,
) {
    if Instant::now() < retry.next_attempt {
        return;
    }
    let pending = match store.pending_projection_sequences(
        config.stream.projection_retry_page,
        config.stream.microbatch_spans,
        config.stream.microbatch_bytes,
    ) {
        Ok(pending) => pending,
        Err(error) => {
            retry.record_failure(shared, error.to_string());
            return;
        }
    };
    if pending.is_empty() {
        retry.record_caught_up(shared);
        return;
    }
    match publish_projections(
        store,
        hub,
        &pending,
        config.stream.delta_history,
        config.blobs.inline_attribute_bytes,
    ) {
        Ok(_) => retry.schedule_now(),
        Err(error) => retry.record_failure(shared, error),
    }
}

fn handle_background_command(
    store: &WorkspaceStore,
    hub: &DeltaHub,
    shared: &WriterShared,
    delta_history: usize,
    command: WriterCommand,
) {
    match command {
        WriterCommand::EnqueueStaleAnalyses {
            definition,
            response,
        } => {
            let result = store
                .enqueue_stale_analyses(&definition)
                .map(|deltas| {
                    publish_deltas(store, hub, deltas, delta_history);
                })
                .map_err(|error| error.to_string());
            let _ = response.send(result);
        }
        WriterCommand::MarkAnalysisStarted { request, response } => {
            let result = store
                .mark_analysis_started(&request)
                .map(|delta| {
                    if let Some(delta) = delta {
                        publish_deltas(store, hub, vec![delta], delta_history);
                        true
                    } else {
                        false
                    }
                })
                .map_err(|error| error.to_string());
            let _ = response.send(result);
        }
        WriterCommand::CommitAnalysis {
            result,
            mut stage_samples,
            response,
        } => {
            let started = Instant::now();
            let finding_count = result.findings.len() as u64;
            let committed = store.commit_analysis(&result);
            let mut sample = PipelineStageSampleV1::new(
                PipelineStageV1::AnalysisCommit,
                started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            );
            sample.item_count = finding_count;
            stage_samples.push(sample);
            record_pipeline_stages(store, shared, &stage_samples);
            let result = committed
                .map(|delta| publish_deltas(store, hub, vec![delta], delta_history))
                .map_err(|error| error.to_string());
            let _ = response.send(result);
        }
        WriterCommand::FailAnalysis {
            request,
            message,
            stage_samples,
            response,
        } => {
            let result = store
                .fail_analysis(&request, &message)
                .map(|delta| {
                    if let Some(delta) = delta {
                        publish_deltas(store, hub, vec![delta], delta_history);
                    }
                })
                .map_err(|error| error.to_string());
            record_pipeline_stages(store, shared, &stage_samples);
            let _ = response.send(result);
        }
        WriterCommand::CommitCohort {
            model,
            project_id,
            analysis_definition_id,
            scope_id,
            history_limit,
            mut stage_samples,
            response,
        } => {
            let started = Instant::now();
            let assignment_count = model.assignments.len() as u64;
            let committed = store.commit_feature_similarity_model_scoped(
                &model,
                &project_id,
                &analysis_definition_id,
                &scope_id,
                history_limit,
            );
            let mut sample = PipelineStageSampleV1::new(
                PipelineStageV1::CohortCommit,
                started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            );
            sample.item_count = if committed.as_ref().is_ok_and(|created| *created) {
                assignment_count
            } else {
                0
            };
            stage_samples.push(sample);
            record_pipeline_stages(store, shared, &stage_samples);
            let _ = response.send(committed.map_err(|error| error.to_string()));
        }
        WriterCommand::AppendCohortAssignments {
            project_id,
            analysis_definition_id,
            scope_id,
            assignments,
            mut stage_samples,
            response,
        } => {
            let started = Instant::now();
            let appended = store.append_active_feature_similarity_assignments(
                &project_id,
                &analysis_definition_id,
                &scope_id,
                &assignments,
            );
            let mut sample = PipelineStageSampleV1::new(
                PipelineStageV1::CohortAssignment,
                started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            );
            sample.item_count = appended.as_ref().copied().unwrap_or(0);
            stage_samples.push(sample);
            record_pipeline_stages(store, shared, &stage_samples);
            let _ = response.send(appended.map_err(|error| error.to_string()));
        }
        WriterCommand::ClaimTopology { response } => {
            let result = store
                .claim_pending_topology()
                .map_err(|error| error.to_string());
            let _ = response.send(result);
        }
        WriterCommand::CommitTopologyChunk {
            job,
            rows,
            first,
            last,
            response,
        } => {
            let started = Instant::now();
            let result = store.commit_topology_chunk(&job, &rows, first, last);
            let mut sample = PipelineStageSampleV1::new(
                PipelineStageV1::Topology,
                started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            );
            sample.item_count = rows.len() as u64;
            sample.rows_scanned = rows.len() as u64;
            record_pipeline_stages(store, shared, &[sample]);
            if let Ok(Some(delta)) = &result {
                hub.publish(delta.clone());
                let _ = store.prune_deltas(delta_history);
            }
            let _ = response.send(result.map(|_| ()).map_err(|error| error.to_string()));
        }
        WriterCommand::FailTopology {
            job,
            message,
            response,
        } => {
            let result = store
                .fail_topology_projection(&job, &message)
                .map_err(|error| error.to_string());
            let _ = response.send(result);
        }
        WriterCommand::Ingest { .. }
        | WriterCommand::CreateProject { .. }
        | WriterCommand::PreviewEvalBatch { .. }
        | WriterCommand::CreateEvalBatch { .. }
        | WriterCommand::CancelEvalBatch { .. }
        | WriterCommand::RetryEvalBatch { .. }
        | WriterCommand::ReviewEvalCandidate { .. }
        | WriterCommand::SetFindingDisposition { .. }
        | WriterCommand::UndoFindingDisposition { .. }
        | WriterCommand::CreateEvalCandidate { .. }
        | WriterCommand::ExecuteCandidateJob { .. }
        | WriterCommand::CommitComparison { .. }
        | WriterCommand::Shutdown(_) => unreachable!("non-analysis command passed to handler"),
    }
}

fn publish_deltas(
    store: &WorkspaceStore,
    hub: &DeltaHub,
    deltas: Vec<TraceDeltaV1>,
    delta_history: usize,
) {
    for delta in deltas {
        hub.publish(delta);
    }
    let _ = store.prune_deltas(delta_history);
}

fn record_pipeline_stages(
    store: &WorkspaceStore,
    shared: &WriterShared,
    samples: &[PipelineStageSampleV1],
) {
    if let Err(error) = store.record_pipeline_stages(samples) {
        shared
            .health
            .lock()
            .expect("health lock poisoned")
            .last_error = Some(format!("could not persist pipeline metrics: {error}"));
    }
}

fn journal_ingest(
    store: &WorkspaceStore,
    shared: &WriterShared,
    config: &PersevalConfigV1,
    mut submission: OtlpSubmission,
    admitted_bytes: usize,
    response: mpsc::Sender<Result<OtlpAdmission, String>>,
) -> Option<(u64, usize, usize)> {
    let accepted = submission.batch.spans.len();
    let rejected = submission.batch.rejected_spans;
    let request_started = submission.request_started;
    let mut stage_samples = std::mem::take(&mut submission.stage_samples);
    let result = store.journal_batch(
        &mut submission.batch,
        &submission.raw_wire_payload,
        &submission.wire_encoding,
        config.blobs.inline_attribute_bytes,
    );
    shared
        .queued_bytes
        .fetch_sub(admitted_bytes, Ordering::AcqRel);
    shared.queued_batches.fetch_sub(1, Ordering::AcqRel);
    match result {
        Ok(receipt) => {
            let duplicate = receipt.duplicate_request;
            let sequence = receipt.journal_sequence;
            stage_samples.extend(receipt.stage_samples);
            let mut health = shared.health.lock().expect("health lock poisoned");
            if !duplicate {
                health.accepted_spans = health.accepted_spans.saturating_add(accepted as u64);
                health.rejected_spans = health.rejected_spans.saturating_add(rejected);
            }
            health.last_error = None;
            drop(health);
            let mut acknowledgement = PipelineStageSampleV1::new(
                PipelineStageV1::DurableAcknowledgement,
                request_started
                    .elapsed()
                    .as_nanos()
                    .min(u128::from(u64::MAX)) as u64,
            );
            acknowledgement.item_count = accepted as u64;
            acknowledgement.byte_count = admitted_bytes as u64;
            stage_samples.push(acknowledgement);
            let _ = response.send(Ok(OtlpAdmission {
                duplicate_request: duplicate,
            }));
            if let Err(error) = store.record_pipeline_stages(&stage_samples) {
                shared
                    .health
                    .lock()
                    .expect("health lock poisoned")
                    .last_error = Some(format!("could not persist pipeline metrics: {error}"));
            }
            Some((sequence, accepted, admitted_bytes))
        }
        Err(error) => {
            let error = error.to_string();
            shared
                .health
                .lock()
                .expect("health lock poisoned")
                .last_error = Some(error.clone());
            let _ = response.send(Err(error));
            None
        }
    }
}

fn publish_projections(
    store: &WorkspaceStore,
    hub: &DeltaHub,
    sequences: &[u64],
    delta_history: usize,
    inline_attribute_bytes: usize,
) -> Result<usize, String> {
    if sequences.is_empty() {
        return Ok(0);
    }
    let deltas = store
        .project_journals_with_inline_limit(sequences, inline_attribute_bytes)
        .map_err(|error| error.to_string())?;
    let delta_count = deltas.len();
    for delta in deltas {
        hub.publish(delta);
    }
    let _ = store.prune_deltas(delta_history);
    Ok(delta_count)
}
