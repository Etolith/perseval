use super::*;

impl FailureInbox {
    pub(super) fn create_candidate(&mut self, cx: &mut Context<Self>) {
        let Some(finding_id) = self.selected_finding_id.clone() else {
            return;
        };
        match self.service.preview_eval_candidate(&finding_id) {
            Ok(candidate) => self.candidate_preview = candidate,
            Err(error) => self.load_error = Some(error.to_string()),
        }
        cx.notify();
    }

    pub(super) fn confirm_candidate(&mut self, cx: &mut Context<Self>) {
        let Some(group_id) = self
            .selected_group
            .as_ref()
            .map(|group| group.summary.group_id.clone())
        else {
            return;
        };
        let Some(finding_id) = self.selected_finding_id.clone() else {
            return;
        };
        match self.service.create_eval_candidate(&group_id, &finding_id) {
            Ok(_) => {
                self.candidate_preview = None;
                self.load_occurrence(&finding_id, cx);
            }
            Err(error) => self.load_error = Some(error.to_string()),
        }
        cx.notify();
    }

    pub(super) fn cancel_candidate(&mut self, cx: &mut Context<Self>) {
        self.candidate_preview = None;
        cx.notify();
    }

    pub(super) fn preview_selected_groups(&mut self, cx: &mut Context<Self>) {
        self.preview_eval_groups(self.selected_group_ids.iter().cloned().collect(), cx);
    }

    pub(super) fn preview_current_group(&mut self, cx: &mut Context<Self>) {
        let groups = self
            .selected_group
            .as_ref()
            .map(|group| vec![group.summary.group_id.clone()])
            .unwrap_or_default();
        self.preview_eval_groups(groups, cx);
    }

    fn preview_eval_groups(&mut self, group_ids: Vec<String>, cx: &mut Context<Self>) {
        let Some(project_id) = self.filters.scope.criteria.project_id.clone() else {
            self.load_error = Some(
                "Choose one project before generating eval candidates; All Projects is read-only."
                    .into(),
            );
            cx.notify();
            return;
        };
        if project_id == UNASSIGNED_PROJECT_ID || group_ids.is_empty() {
            self.load_error = Some(
                "Choose one or more groups in a persisted project before generating eval candidates."
                    .into(),
            );
            cx.notify();
            return;
        }
        let service = self.service.clone();
        let scope = self.filters.scope.clone();
        self.batch_loading = true;
        self.batch_preview = None;
        self.expanded_eval_group_ids.clear();
        self.generation_job = None;
        self.load_error = None;
        let task = cx.background_spawn(async move {
            service.preview_eval_batch(
                &project_id,
                &EvalBatchSelectionSpecV1 {
                    scope,
                    group_ids,
                    policy: Default::default(),
                },
            )
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.batch_loading = false;
                match result {
                    Ok(preview) => this.batch_preview = Some(preview),
                    Err(error) => this.load_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn create_eval_batch(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.batch_preview.clone() else {
            return;
        };
        let service = self.service.clone();
        self.batch_loading = true;
        self.load_error = None;
        let idempotency_key = format!("desktop:{}", preview.preview_id);
        let task = cx.background_spawn(async move {
            service.create_eval_batch(
                &preview.project_id,
                &preview.preview_id,
                &preview.selection_hash,
                &idempotency_key,
            )
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.batch_loading = false;
                match result {
                    Ok(job) => {
                        let job_id = job.job_id.clone();
                        this.generation_job = Some(job);
                        this.selected_group_ids.clear();
                        this.poll_generation_job(job_id, cx);
                    }
                    Err(error) => this.load_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn poll_generation_job(&mut self, job_id: String, cx: &mut Context<Self>) {
        let service = self.service.clone();
        let requested_job_id = job_id.clone();
        let executor = cx.background_executor().clone();
        let task = cx.background_spawn(async move {
            executor.timer(Duration::from_millis(100)).await;
            service.get_candidate_generation_job(&requested_job_id)
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| match result {
                Ok(Some(job)) => {
                    let running = matches!(
                        job.status,
                        CandidateGenerationJobStatusV1::Queued
                            | CandidateGenerationJobStatusV1::Running
                    );
                    this.generation_job = Some(job);
                    if running {
                        this.poll_generation_job(job_id, cx);
                    }
                    cx.notify();
                }
                Ok(None) => {
                    this.load_error = Some("Candidate generation job disappeared.".into());
                    cx.notify();
                }
                Err(error) => {
                    this.load_error = Some(error.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn cancel_generation_job(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.generation_job.as_ref() else {
            self.close_eval_batch_preview(cx);
            return;
        };
        if !matches!(
            job.status,
            CandidateGenerationJobStatusV1::Queued | CandidateGenerationJobStatusV1::Running
        ) {
            self.close_eval_batch_preview(cx);
            return;
        }
        let job_id = job.job_id.clone();
        let service = self.service.clone();
        let task = cx.background_spawn(async move { service.cancel_eval_batch(&job_id) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                match result {
                    Ok(job) => this.generation_job = Some(job),
                    Err(error) => this.load_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn retry_generation_job(&mut self, cx: &mut Context<Self>) {
        let Some(job_id) = self.generation_job.as_ref().map(|job| job.job_id.clone()) else {
            return;
        };
        let service = self.service.clone();
        let task = cx.background_spawn(async move { service.retry_eval_batch(&job_id) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                match result {
                    Ok(job) => {
                        let job_id = job.job_id.clone();
                        this.generation_job = Some(job);
                        this.poll_generation_job(job_id, cx);
                    }
                    Err(error) => this.load_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn close_eval_batch_preview(&mut self, cx: &mut Context<Self>) {
        self.batch_preview = None;
        self.expanded_eval_group_ids.clear();
        self.generation_job = None;
        cx.notify();
    }

    pub(super) fn open_eval_queue(&mut self, cx: &mut Context<Self>) {
        cx.emit(FailureInboxEvent::OpenEvalQueue);
    }
}
