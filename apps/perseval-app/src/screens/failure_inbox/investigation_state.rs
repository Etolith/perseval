use super::*;

impl FailureInbox {
    pub(super) fn load_group(&mut self, project_id: &str, group_id: &str, cx: &mut Context<Self>) {
        self.request_group(project_id, group_id, 0, None, None, cx);
    }

    pub(super) fn request_group(
        &mut self,
        project_id: &str,
        group_id: &str,
        occurrence_offset: u64,
        preferred_finding_id: Option<String>,
        preferred_span_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.occurrence_offset = occurrence_offset;
        self.compare_base_finding_id = None;
        self.diagnosis_details_open = false;
        self.investigation_actions_open = false;
        self.finding_review_open = false;
        self.investigation_loading = true;
        self.evidence_loading = false;
        self.evidence_request_generation = self.evidence_request_generation.wrapping_add(1);
        self.pending_focus_span_id = preferred_span_id;
        self.load_error = None;

        let scope = self.scope_for_group(project_id, group_id);
        let service = self.service.clone();
        let group_id = group_id.to_owned();
        self.investigation_request_generation =
            self.investigation_request_generation.wrapping_add(1);
        let generation = self.investigation_request_generation;
        let task = cx.background_spawn(async move {
            let group = service.get_failure_group_in_scope(&scope, &group_id)?;
            let occurrences = service.list_failure_occurrences_in_scope(
                &scope,
                &group_id,
                occurrence_offset,
                100,
            )?;
            Ok::<_, perseval_service::LiveServiceError>((group, occurrences))
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.investigation_request_generation != generation {
                    return;
                }
                this.investigation_loading = false;
                match result {
                    Ok((group, occurrences)) => {
                        this.selected_group = group;
                        this.occurrences = occurrences;
                        this.load_error = None;
                        let finding_id = preferred_finding_id
                            .filter(|finding_id| {
                                this.occurrences
                                    .iter()
                                    .any(|occurrence| occurrence.finding.finding_id == *finding_id)
                            })
                            .or_else(|| {
                                this.occurrences
                                    .first()
                                    .map(|occurrence| occurrence.finding.finding_id.clone())
                            });
                        if let Some(finding_id) = finding_id {
                            this.load_occurrence(&finding_id, cx);
                        } else {
                            this.selected_finding_id = None;
                            this.evidence = None;
                            this.focused_span_id = None;
                            this.focused_span_snapshot = None;
                            this.pending_focus_span_id = None;
                        }
                    }
                    Err(error) => this.load_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn load_occurrence(&mut self, finding_id: &str, cx: &mut Context<Self>) {
        let Some((scope, group_id)) = self
            .selected_group
            .as_ref()
            .map(|group| (group.summary.scope.clone(), group.summary.group_id.clone()))
        else {
            return;
        };
        let service = self.service.clone();
        let finding_id = finding_id.to_owned();
        self.evidence_loading = true;
        self.evidence_request_generation = self.evidence_request_generation.wrapping_add(1);
        let generation = self.evidence_request_generation;
        self.candidate_preview = None;
        self.diagnosis_details_open = false;
        self.revealed_payload = None;
        let requested_finding_id = finding_id.clone();
        let task = cx.background_spawn(async move {
            service.get_finding_evidence_in_scope(&scope, &group_id, &requested_finding_id)
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.evidence_request_generation != generation {
                    return;
                }
                this.evidence_loading = false;
                match result {
                    Ok(Some(evidence)) => {
                        this.selected_finding_id = Some(finding_id);
                        this.evidence = Some(evidence);
                        this.load_error = None;
                        this.focused_span_id = this.pending_focus_span_id.take().or_else(|| {
                            this.evidence
                                .as_ref()
                                .and_then(|evidence| evidence.evidence_span_ids.first().cloned())
                        });
                        this.focused_span_snapshot = this.evidence.as_ref().and_then(|evidence| {
                            let id = this.focused_span_id.as_deref()?;
                            evidence
                                .spans
                                .iter()
                                .find(|span| span.span_id == id)
                                .cloned()
                        });
                        this.tab = InspectorTab::Finding;
                    }
                    Ok(None) => {
                        this.pending_focus_span_id = None;
                        this.load_error = Some(
                            "The selected example is no longer available in this failure-group snapshot. The previous example is still shown; refresh the group to reconcile live analysis updates."
                                .into(),
                        );
                    }
                    Err(error) => this.load_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn show_inbox(&mut self, cx: &mut Context<Self>) {
        self.showing_inbox = true;
        self.investigation_target = None;
        self.full_trace = false;
        self.compare_base_finding_id = None;
        self.investigation_actions_open = false;
        self.finding_review_open = false;
        cx.notify();
    }

    pub(crate) fn show_investigation(
        &mut self,
        project_id: &str,
        group_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.showing_inbox = false;
        self.full_trace = false;
        self.investigation_target = Some((project_id.to_owned(), group_id.to_owned()));
        if self.selected_group.as_ref().is_none_or(|group| {
            group.summary.project_id != project_id || group.summary.group_id != group_id
        }) {
            self.load_group(project_id, group_id, cx);
        }
        cx.notify();
    }

    pub(super) fn scope_for_group(&self, project_id: &str, group_id: &str) -> QueryScopeV1 {
        self.groups
            .iter()
            .find(|group| group.project_id == project_id && group.group_id == group_id)
            .map(|group| group.scope.clone())
            .or_else(|| {
                self.selected_group
                    .as_ref()
                    .filter(|group| {
                        group.summary.project_id == project_id && group.summary.group_id == group_id
                    })
                    .map(|group| group.summary.scope.clone())
            })
            .unwrap_or_else(|| {
                let mut criteria = self.filters.scope.criteria.clone();
                criteria.project_id = Some(project_id.to_owned());
                QueryScopeV1::new(criteria)
            })
    }

    pub(crate) fn restore_investigation_context(
        &mut self,
        project_id: &str,
        group_id: &str,
        finding_id: Option<&str>,
        occurrence_offset: u64,
        span_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.showing_inbox = false;
        self.full_trace = false;
        self.investigation_target = Some((project_id.to_owned(), group_id.to_owned()));
        self.request_group(
            project_id,
            group_id,
            occurrence_offset,
            finding_id.map(str::to_owned),
            span_id.map(str::to_owned),
            cx,
        );
    }

    pub(super) fn open_group(
        &mut self,
        project_id: String,
        group_id: String,
        cx: &mut Context<Self>,
    ) {
        self.show_investigation(&project_id, &group_id, cx);
        cx.emit(FailureInboxEvent::OpenInvestigation {
            project_id,
            group_id,
        });
    }

    pub(super) fn open_focused_group(&mut self, cx: &mut Context<Self>) {
        if !self.showing_inbox {
            return;
        }
        if let Some((project_id, group_id)) = self.focused_group.clone() {
            self.open_group(project_id, group_id, cx);
        }
    }

    pub(super) fn select_occurrence(&mut self, finding_id: String, cx: &mut Context<Self>) {
        self.pending_focus_span_id = None;
        self.load_occurrence(&finding_id, cx);
    }

    pub(super) fn selected_occurrence_index(&self) -> Option<usize> {
        let selected = self.selected_finding_id.as_deref()?;
        self.occurrences
            .iter()
            .position(|occurrence| occurrence.finding.finding_id == selected)
    }

    pub(super) fn occurrence_position(&self) -> Option<u64> {
        self.selected_occurrence_index()
            .map(|index| self.occurrence_offset + index as u64 + 1)
    }

    pub(super) fn can_navigate_occurrence(&self, forward: bool) -> bool {
        if self.investigation_loading || self.evidence_loading {
            return false;
        }
        let Some(index) = self.selected_occurrence_index() else {
            return false;
        };
        let (previous, next) = occurrence_navigation_state(
            self.occurrence_offset,
            index,
            self.occurrences.len(),
            self.selected_group
                .as_ref()
                .map(|group| group.summary.occurrence_count)
                .unwrap_or_default(),
        );
        if forward { next } else { previous }
    }

    pub(super) fn navigate_occurrence(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some((scope, group_id)) = self
            .selected_group
            .as_ref()
            .map(|group| (group.summary.scope.clone(), group.summary.group_id.clone()))
        else {
            return;
        };
        let Some(index) = self.selected_occurrence_index() else {
            return;
        };
        let next_index = if forward {
            index.checked_add(1)
        } else {
            index.checked_sub(1)
        };
        if let Some(next_index) = next_index
            && let Some(finding_id) = self
                .occurrences
                .get(next_index)
                .map(|occurrence| occurrence.finding.finding_id.clone())
        {
            self.pending_focus_span_id = None;
            self.load_occurrence(&finding_id, cx);
            return;
        }

        let total = self
            .selected_group
            .as_ref()
            .map(|group| group.summary.occurrence_count)
            .unwrap_or_default();
        let new_offset = if forward {
            let offset = self.occurrence_offset + self.occurrences.len() as u64;
            (offset < total).then_some(offset)
        } else {
            (self.occurrence_offset > 0).then_some(self.occurrence_offset.saturating_sub(100))
        };
        let Some(new_offset) = new_offset else {
            return;
        };

        self.investigation_loading = true;
        self.investigation_request_generation =
            self.investigation_request_generation.wrapping_add(1);
        let generation = self.investigation_request_generation;
        let service = self.service.clone();
        let task = cx.background_spawn(async move {
            service.list_failure_occurrences_in_scope(&scope, &group_id, new_offset, 100)
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.investigation_request_generation != generation {
                    return;
                }
                this.investigation_loading = false;
                match result {
                    Ok(occurrences) => {
                        let finding_id = if forward {
                            occurrences.first()
                        } else {
                            occurrences.last()
                        }
                        .map(|occurrence| occurrence.finding.finding_id.clone());
                        this.occurrence_offset = new_offset;
                        this.occurrences = occurrences;
                        if let Some(finding_id) = finding_id {
                            this.pending_focus_span_id = None;
                            this.load_occurrence(&finding_id, cx);
                        }
                    }
                    Err(error) => this.load_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
