use std::sync::Arc;

use gpui::{
    AnyElement, AppContext, Context, Div, Entity, FontWeight, IntoElement, Render, Role, Window,
    div, prelude::*, px,
};
use perseval_service::{
    CalibrationReleaseV1, CalibrationReportV1, CalibrationSliceReportV1, LiveTraceService,
    ThresholdPolicyReleaseV1,
};

use crate::components::{TextInput, button_state, tag};
use crate::design::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricView {
    Confusion,
    Agreement,
    Calibration,
    RiskCoverage,
}

struct AutomationGateItem {
    label: &'static str,
    current: String,
    passed: bool,
}

pub(crate) struct CalibrationScreen {
    service: Arc<LiveTraceService>,
    project_id: Option<String>,
    releases: Vec<(String, CalibrationReleaseV1)>,
    selected_release_id: Option<String>,
    reports: Vec<CalibrationReportV1>,
    metric_view: MetricView,
    pass_threshold: Entity<TextInput>,
    fail_threshold: Entity<TextInput>,
    minimum_confidence: Entity<TextInput>,
    proposed_policy: Option<(String, ThresholdPolicyReleaseV1)>,
    busy: bool,
    error: Option<String>,
    notice: Option<String>,
    request_generation: u64,
}

impl CalibrationScreen {
    pub(crate) fn new(
        service: Arc<LiveTraceService>,
        project_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let pass_threshold = input("0.35", "Pass threshold", cx);
        let fail_threshold = input("0.65", "Failure threshold", cx);
        let minimum_confidence = input("0.60", "Minimum confidence", cx);
        let mut this = Self {
            service,
            project_id,
            releases: Vec::new(),
            selected_release_id: None,
            reports: Vec::new(),
            metric_view: MetricView::Confusion,
            pass_threshold,
            fail_threshold,
            minimum_confidence,
            proposed_policy: None,
            busy: false,
            error: None,
            notice: None,
            request_generation: 0,
        };
        this.reload(cx);
        this
    }

    pub(crate) fn set_project_scope(&mut self, project_id: Option<String>, cx: &mut Context<Self>) {
        if self.project_id == project_id {
            return;
        }
        self.project_id = project_id;
        self.selected_release_id = None;
        self.reports.clear();
        self.proposed_policy = None;
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.request_generation = self.request_generation.wrapping_add(1);
        let generation = self.request_generation;
        self.error = None;
        let Some(project_id) = self.project_id.clone() else {
            self.releases.clear();
            cx.notify();
            return;
        };
        self.busy = true;
        let service = self.service.clone();
        let task = cx
            .background_spawn(async move { service.list_calibration_releases(&project_id, None) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.request_generation != generation {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(releases) => {
                        this.releases = releases;
                        if this.selected_release_id.is_none() {
                            this.selected_release_id =
                                this.releases.first().map(|(id, _)| id.clone());
                        }
                        this.load_reports(cx);
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn select_release(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((release_id, _)) = self.releases.get(index) else {
            return;
        };
        self.selected_release_id = Some(release_id.clone());
        self.proposed_policy = None;
        self.notice = None;
        self.load_reports(cx);
    }

    fn load_reports(&mut self, cx: &mut Context<Self>) {
        let Some(release_id) = self.selected_release_id.clone() else {
            self.reports.clear();
            return;
        };
        let expected_release_id = release_id.clone();
        self.busy = true;
        let service = self.service.clone();
        let task = cx.background_spawn(async move {
            Ok::<_, perseval_service::LiveServiceError>((
                service.list_calibration_reports(&release_id)?,
                service.threshold_policy_for_calibration(&release_id)?,
            ))
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.selected_release_id.as_deref() != Some(expected_release_id.as_str()) {
                    return;
                }
                this.busy = false;
                match result {
                    Ok((reports, policy)) => {
                        this.reports = reports;
                        if let Some((_, frozen)) = &policy {
                            this.pass_threshold.update(cx, |input, cx| {
                                input.set_text(
                                    format!("{:.3}", frozen.pass_probability_threshold),
                                    cx,
                                )
                            });
                            this.fail_threshold.update(cx, |input, cx| {
                                input.set_text(
                                    format!("{:.3}", frozen.fail_probability_threshold),
                                    cx,
                                )
                            });
                            this.minimum_confidence.update(cx, |input, cx| {
                                input.set_text(
                                    format!("{:.3}", frozen.minimum_decision_confidence),
                                    cx,
                                )
                            });
                        }
                        this.proposed_policy = policy;
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn create_policy(&mut self, cx: &mut Context<Self>) {
        if self.held_out_report().is_some() {
            self.error = Some(
                "Held-out labels are already unsealed. Create a new calibration release instead of retuning this policy.".into(),
            );
            cx.notify();
            return;
        }
        if self.proposed_policy.is_some() {
            self.error =
                Some("This calibration release already has a frozen threshold policy.".into());
            cx.notify();
            return;
        }
        let Some((calibration_release_id, release)) = self
            .selected_release()
            .map(|(id, release)| (id.to_owned(), release.clone()))
        else {
            return;
        };
        if !agreement_is_sufficient(&release) {
            self.error =
                Some("Threshold publication requires Krippendorff alpha of at least 0.67.".into());
            cx.notify();
            return;
        }
        let Some(project_id) = self.project_id.clone() else {
            return;
        };
        let pass = self.pass_threshold.read(cx).text().trim().parse::<f64>();
        let fail = self.fail_threshold.read(cx).text().trim().parse::<f64>();
        let confidence = self
            .minimum_confidence
            .read(cx)
            .text()
            .trim()
            .parse::<f64>();
        let (Ok(pass), Ok(fail), Ok(confidence)) = (pass, fail, confidence) else {
            self.error = Some("Thresholds must be probabilities between 0 and 1.".into());
            cx.notify();
            return;
        };
        self.busy = true;
        self.error = None;
        let service = self.service.clone();
        let evaluator = release.evaluator_release_id.clone();
        let expected_release_id = calibration_release_id.clone();
        let task = cx.background_spawn(async move {
            service.publish_threshold_policy_release(
                &project_id,
                &evaluator,
                &calibration_release_id,
                pass,
                fail,
                confidence,
            )
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.selected_release_id.as_deref() != Some(expected_release_id.as_str()) {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(policy) => {
                        this.proposed_policy = Some(policy);
                        this.notice = Some("Threshold policy frozen from calibration data. You can now run the one-shot held-out evaluation.".into());
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn fit_calibration(&mut self, cx: &mut Context<Self>) {
        let Some(project_id) = self.project_id.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let service = self.service.clone();
        let task =
            cx.background_spawn(async move { service.fit_latest_review_calibration(&project_id) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok((release_id, _)) => {
                        this.selected_release_id = Some(release_id);
                        this.notice = Some(
                            "Published a calibration release. Freeze thresholds before opening held-out results."
                                .into(),
                        );
                        this.reload(cx);
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn evaluate_held_out(&mut self, cx: &mut Context<Self>) {
        let Some(calibration_release_id) = self.selected_release_id.clone() else {
            return;
        };
        if self.proposed_policy.is_none() || self.held_out_report().is_some() {
            return;
        }
        self.busy = true;
        self.error = None;
        let expected_release_id = calibration_release_id.clone();
        let service = self.service.clone();
        let task = cx.background_spawn(async move {
            service.publish_calibration_test_report(&calibration_release_id)
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.selected_release_id.as_deref() != Some(expected_release_id.as_str()) {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(_) => {
                        this.notice = Some(
                            "Held-out evaluation published once against the frozen policy. Thresholds are now locked."
                                .into(),
                        );
                        this.load_reports(cx);
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn activate_policy(&mut self, cx: &mut Context<Self>) {
        let Some((policy_id, _)) = self.proposed_policy.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let service = self.service.clone();
        let task = cx.background_spawn(async move {
            service.activate_threshold_policy_and_materialize(&policy_id)
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok((activation, decision_count)) => this.notice = Some(format!(
                        "Policy activated as {} and materialized {decision_count} immutable decisions. Prior assessments were not changed.",
                        short_id(&activation.activation_id),
                    )),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn selected_release(&self) -> Option<(&str, &CalibrationReleaseV1)> {
        let id = self.selected_release_id.as_deref()?;
        self.releases
            .iter()
            .find(|(release_id, _)| release_id == id)
            .map(|(release_id, release)| (release_id.as_str(), release))
    }

    fn held_out_report(&self) -> Option<&perseval_service::BinaryCalibrationReportV1> {
        self.reports.first().map(|report| &report.report)
    }

    fn displayed_slice_reports(&self) -> &[CalibrationSliceReportV1] {
        if let Some(report) = self.reports.first() {
            return report.slice_reports.as_slice();
        }
        if let Some((_, release)) = self.selected_release() {
            return release.fit_slice_reports.as_slice();
        }
        &[]
    }

    fn displayed_report(&self) -> Option<&perseval_service::BinaryCalibrationReportV1> {
        self.reports
            .first()
            .and_then(|report| random_audit_report(&report.slice_reports).or(Some(&report.report)))
            .or_else(|| {
                self.selected_release()
                    .map(|(_, release)| &release.fit_report)
            })
    }

    fn displayed_report_scope(&self) -> &'static str {
        if self
            .reports
            .first()
            .is_some_and(|report| random_audit_report(&report.slice_reports).is_some())
        {
            "Metrics scope: held-out random-audit slice · used by automation safety gates"
        } else if self.held_out_report().is_some() {
            "Metrics scope: complete held-out report · no random-audit slice available"
        } else {
            "Metrics scope: calibration fit"
        }
    }

    fn render_release_list(&self, cx: &mut Context<Self>) -> Div {
        let rows = self.releases.iter().enumerate().fold(
            div().flex().flex_col(),
            |rows, (index, (id, release))| {
                let selected = self.selected_release_id.as_deref() == Some(id.as_str());
                rows.child(
                    div()
                        .id(("calibration-release", index))
                        .role(Role::Button)
                        .aria_label(format!(
                            "Open calibration release {}; {} fitted answers; {} reviewers",
                            short_id(id),
                            release.fit_annotation_revision_ids.len(),
                            release.agreement_report.rater_count
                        ))
                        .tab_index(0)
                        .focus_visible(|style| style.border_2().border_color(Theme::FOCUS_RING))
                        .cursor_pointer()
                        .px_3()
                        .py_3()
                        .border_b_1()
                        .border_color(Theme::BORDER)
                        .when(selected, |row| row.bg(Theme::SELECTED))
                        .on_click(cx.listener(move |this, _, _, cx| this.select_release(index, cx)))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("Release {}", short_id(id))),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(Theme::MUTED)
                                .child(format!(
                                    "{} fitted answers · {} reviewers",
                                    release.fit_annotation_revision_ids.len(),
                                    release.agreement_report.rater_count
                                )),
                        ),
                )
            },
        );
        div()
            .w(px(285.))
            .flex_none()
            .border_r_1()
            .border_color(Theme::BORDER)
            .child(
                div()
                    .px_4()
                    .py_3()
                    .text_xs()
                    .text_color(Theme::MUTED)
                    .child(format!("{} releases", self.releases.len())),
            )
            .child(rows)
    }

    fn render_metrics(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some((release_id, release)) = self.selected_release() else {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(Theme::MUTED)
                .child("No calibration release yet. Resolve blind calibration cases first.")
                .child(
                    button_state("Fit reviewed answers", true, !self.busy)
                        .id("fit-review-calibration")
                        .role(Role::Button)
                        .aria_label("Fit a calibration release from resolved blind reviews")
                        .mt_4()
                        .on_click(cx.listener(|this, _, _, cx| this.fit_calibration(cx))),
                )
                .into_any_element();
        };
        let report = self.displayed_report();
        let held_out = self.held_out_report().is_some();
        let content = match self.metric_view {
            MetricView::Confusion => report.map_or_else(
                || metric_empty("Resolve reviewed cases to compute confusion metrics."),
                |report| {
                    div()
                        .child(metric_heading("Positive class", &report.positive_class))
                        .child(
                            div()
                                .mt_4()
                                .flex()
                                .flex_wrap()
                                .gap_3()
                                .child(metric(
                                    "True positive",
                                    report.confusion.true_positive.to_string(),
                                ))
                                .child(metric(
                                    "False positive",
                                    report.confusion.false_positive.to_string(),
                                ))
                                .child(metric(
                                    "True negative",
                                    report.confusion.true_negative.to_string(),
                                ))
                                .child(metric(
                                    "False negative",
                                    report.confusion.false_negative.to_string(),
                                )),
                        )
                        .child(
                            div()
                                .mt_4()
                                .flex()
                                .flex_wrap()
                                .gap_3()
                                .child(metric(
                                    "Precision · 95% CI",
                                    format_rate_interval(
                                        report.precision,
                                        report.precision_interval.as_ref().map(|interval| {
                                            (
                                                interval.lower_95,
                                                interval.upper_95,
                                                interval.successes,
                                                interval.trials,
                                            )
                                        }),
                                    ),
                                ))
                                .child(metric(
                                    "Recall · 95% CI",
                                    format_rate_interval(
                                        report.recall,
                                        report.recall_interval.as_ref().map(|interval| {
                                            (
                                                interval.lower_95,
                                                interval.upper_95,
                                                interval.successes,
                                                interval.trials,
                                            )
                                        }),
                                    ),
                                ))
                                .child(metric(
                                    "Specificity · 95% CI",
                                    format_rate_interval(
                                        report.specificity,
                                        report.specificity_interval.as_ref().map(|interval| {
                                            (
                                                interval.lower_95,
                                                interval.upper_95,
                                                interval.successes,
                                                interval.trials,
                                            )
                                        }),
                                    ),
                                )),
                        )
                        .child(
                            div()
                                .mt_4()
                                .flex()
                                .flex_wrap()
                                .gap_3()
                                .child(metric("F1", format_optional(report.f1)))
                                .child(metric("MCC", format_optional(report.matthews_correlation)))
                                .child(metric("AUPRC", format_optional(report.average_precision)))
                                .child(metric("Denominator", report.attempted_count.to_string())),
                        )
                },
            ),
            MetricView::Agreement => {
                let agreement = &release.agreement_report;
                div().child(metric_heading("Independent reviewers", &agreement.rater_count.to_string()))
                    .child(div().mt_4().flex().flex_wrap().gap_3()
                        .child(metric("Krippendorff α", format_optional(agreement.krippendorff_alpha)))
                        .child(metric("Cohen κ", format_optional(agreement.cohen_kappa)))
                        .child(metric(
                            "Weighted κ",
                            format_optional(
                                release
                                    .ordinal_agreement_report
                                    .as_ref()
                                    .and_then(|report| report.quadratic_weighted_kappa),
                            ),
                        ))
                        .child(metric("Units", agreement.item_count.to_string()))
                        .child(metric("Disagreements", agreement.disagreement_item_count.to_string())))
                    .child(div().mt_5().text_sm().text_color(Theme::MUTED)
                        .child("Low agreement is a rubric-repair signal; it never promotes automated findings."))
            }
            MetricView::Calibration => report.map_or_else(
                || metric_empty("Resolve reviewed cases to compute probability calibration."),
                |report| {
                    div()
                        .child(
                            div()
                                .flex()
                                .gap_3()
                                .child(metric("Brier score", format_optional(report.brier_score)))
                                .child(metric(
                                    "ECE",
                                    format_optional(report.expected_calibration_error),
                                ))
                                .child(metric("Bins", report.calibration_bins.len().to_string())),
                        )
                        .child(
                            div()
                                .mt_5()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(Theme::MUTED)
                                .child("RELIABILITY BINS"),
                        )
                        .child(
                            div()
                                .mt_2()
                                .children(report.calibration_bins.iter().map(|bin| {
                                    div()
                                        .py_2()
                                        .border_b_1()
                                        .border_color(Theme::BORDER)
                                        .flex()
                                        .justify_between()
                                        .text_sm()
                                        .child(format!(
                                            "{:.2}–{:.2}",
                                            bin.lower_bound_inclusive, bin.upper_bound
                                        ))
                                        .child(format!(
                                            "n={} · predicted {} · observed {}",
                                            bin.count,
                                            format_optional(bin.mean_predicted_failure),
                                            format_optional(bin.empirical_failure_rate)
                                        ))
                                })),
                        )
                },
            ),
            MetricView::RiskCoverage => {
                report.map_or_else(
                    || metric_empty("Resolve reviewed cases to compute selective-risk analysis."),
                    |report| {
                        div()
                            .child(
                                div()
                                    .flex()
                                    .gap_3()
                                    .child(metric("Attempted", report.attempted_count.to_string()))
                                    .child(metric("Decided", report.decided_count.to_string()))
                                    .child(metric("Abstained", report.abstained_count.to_string())),
                            )
                            .child(
                                div()
                                    .mt_5()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(Theme::MUTED)
                                    .child("SELECTIVE RISK"),
                            )
                            .child(div().mt_2().children(
                                report.selective_risk.iter().take(20).map(|point| {
                                    div()
                                        .py_2()
                                        .border_b_1()
                                        .border_color(Theme::BORDER)
                                        .flex()
                                        .justify_between()
                                        .text_sm()
                                        .child(format!("Coverage {:.1}%", point.coverage * 100.0))
                                        .child(format!(
                                            "Risk {} · n={}",
                                            format_optional(point.classification_risk),
                                            point.decided_count
                                        ))
                                }),
                            ))
                    },
                )
            }
        };
        div()
            .id("calibration-workbench-scroll")
            .flex_1()
            .min_w_0()
            .p_6()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .child(
                        div()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Calibration workbench"),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_sm()
                                    .text_color(Theme::MUTED)
                                    .child(format!(
                                        "Release {} · {}",
                                        short_id(release_id),
                                        if held_out {
                                            "HELD-OUT TEST · frozen policy"
                                        } else {
                                            "CALIBRATION FIT · choose thresholds before test"
                                        }
                                    )),
                            ),
                    )
                    .child(tag(
                        &format!("Positive class: {}", release.fit_report.positive_class),
                        Theme::AMBER,
                    )),
            )
            .when_some(
                self.render_automation_gate_summary(release),
                |workbench, summary| workbench.child(summary),
            )
            .child(
                div().mt_5().flex().flex_wrap().gap_2().children(
                    [
                        (MetricView::Confusion, "Confusion"),
                        (MetricView::Agreement, "Agreement"),
                        (MetricView::Calibration, "Calibration"),
                        (MetricView::RiskCoverage, "Risk & coverage"),
                    ]
                    .into_iter()
                    .map(|(view, label)| {
                        button_state(label, self.metric_view == view, !self.busy)
                            .id(("calibration-metric-view", metric_view_ordinal(view)))
                            .role(Role::Button)
                            .aria_label(format!("Show {label} metrics"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.metric_view = view;
                                cx.notify();
                            }))
                    }),
                ),
            )
            .child(
                div()
                    .mt_3()
                    .text_sm()
                    .text_color(Theme::MUTED)
                    .child(self.displayed_report_scope()),
            )
            .child(div().mt_5().child(content))
            .child(self.render_slice_performance())
            .child(self.render_threshold_policy(cx))
            .into_any_element()
    }

    fn automation_gate_items(
        &self,
        release: &CalibrationReleaseV1,
    ) -> Option<Vec<AutomationGateItem>> {
        let held_out = self.reports.first()?;
        let fit = random_audit_report(&release.fit_slice_reports)?;
        let test = random_audit_report(&held_out.slice_reports)?;
        let label_count = fit.attempted_count.saturating_add(test.attempted_count);
        let positive_count = fit
            .confusion
            .true_positive
            .saturating_add(fit.confusion.false_negative)
            .saturating_add(test.confusion.true_positive)
            .saturating_add(test.confusion.false_negative);
        let negative_count = fit
            .confusion
            .true_negative
            .saturating_add(fit.confusion.false_positive)
            .saturating_add(test.confusion.true_negative)
            .saturating_add(test.confusion.false_positive);
        let decision_coverage = if test.attempted_count == 0 {
            0.0
        } else {
            test.decided_count as f64 / test.attempted_count as f64
        };
        let grouped_lower = test
            .macro_f1_interval
            .as_ref()
            .map(|interval| interval.lower_95);
        let quality_passed = automation_quality_metrics_sufficient(test);
        Some(vec![
            AutomationGateItem {
                label: "Unbiased random labels",
                current: format!("{label_count} / 500"),
                passed: label_count >= 500,
            },
            AutomationGateItem {
                label: "Each important class",
                current: format!("{positive_count}/100 failure · {negative_count}/100 non-failure"),
                passed: positive_count >= 100 && negative_count >= 100,
            },
            AutomationGateItem {
                label: "Held-out coverage",
                current: format!("{:.1}% / 90%", decision_coverage * 100.0),
                passed: decision_coverage >= 0.90,
            },
            AutomationGateItem {
                label: "Grouped confidence",
                current: grouped_lower.map_or_else(
                    || "not available / >0.053".into(),
                    |lower| format!("{lower:.3} / >0.053"),
                ),
                passed: grouped_lower.is_some_and(|lower| lower > 0.053),
            },
            AutomationGateItem {
                label: "Quality and calibration",
                current: format!(
                    "macro F1 {} · MCC {} · AP {} · Brier {} · ECE {}",
                    format_optional(test.macro_f1),
                    format_optional(test.matthews_correlation),
                    format_optional(test.average_precision),
                    format_optional(test.brier_score),
                    format_optional(test.expected_calibration_error),
                ),
                passed: quality_passed,
            },
        ])
    }

    fn render_automation_gate_summary(&self, release: &CalibrationReleaseV1) -> Option<AnyElement> {
        let items = self.automation_gate_items(release)?;
        let passed = items.iter().filter(|item| item.passed).count();
        let ready = passed == items.len();
        let accessible_summary = items
            .iter()
            .map(|item| {
                format!(
                    "{}: {}, {}",
                    item.label,
                    item.current,
                    if item.passed { "passed" } else { "blocked" }
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        Some(
            div()
                .id("automation-safety-gate-summary")
                .mt_4()
                .p_4()
                .rounded_sm()
                .border_1()
                .border_color(if ready { Theme::GREEN } else { Theme::AMBER })
                .bg(if ready {
                    Theme::SUCCESS_SURFACE
                } else {
                    Theme::WARNING_SURFACE
                })
                .role(Role::Alert)
                .aria_label(format!(
                    "Automation {}. {passed} of {} safety gates pass. {accessible_summary}",
                    if ready { "ready" } else { "blocked" },
                    items.len()
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(if ready {
                                    "Automation ready"
                                } else {
                                    "Automation blocked"
                                }),
                        )
                        .child(tag(
                            &format!("{passed}/{} gates pass", items.len()),
                            if ready { Theme::GREEN } else { Theme::AMBER },
                        )),
                )
                .child(
                    div()
                        .mt_3()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .children(items.into_iter().map(|item| {
                            div()
                                .min_w(px(180.))
                                .flex_1()
                                .p_3()
                                .rounded_sm()
                                .bg(Theme::PANEL_SURFACE)
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(if item.passed {
                                            Theme::GREEN
                                        } else {
                                            Theme::AMBER
                                        })
                                        .child(format!(
                                            "{} · {}",
                                            if item.passed { "PASS" } else { "BLOCKED" },
                                            item.label
                                        )),
                                )
                                .child(
                                    div()
                                        .mt_1()
                                        .text_xs()
                                        .text_color(Theme::MUTED)
                                        .child(item.current),
                                )
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_slice_performance(&self) -> Div {
        let slices = self.displayed_slice_reports();
        div()
            .mt_8()
            .pt_6()
            .border_t_1()
            .border_color(Theme::BORDER)
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Slice performance"),
            )
            .child(
                div()
                    .mt_1()
                    .text_sm()
                    .text_color(Theme::MUTED)
                    .child(
                        "Build, environment, language, and domain come from each exact reviewed revision. Selection-stream rows keep random-audit population evidence separate from actively selected reviews.",
                    ),
            )
            .when(slices.is_empty(), |section| {
                section.child(
                    div()
                        .mt_4()
                        .text_sm()
                        .text_color(Theme::MUTED)
                        .child("No reproducible slice report is available for this release."),
                )
            })
            .when(!slices.is_empty(), |section| {
                section
                    .child(
                        div()
                            .mt_4()
                            .pb_2()
                            .border_b_1()
                            .border_color(Theme::BORDER)
                            .flex()
                            .gap_3()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(Theme::MUTED)
                            .child(div().w(px(110.)).child("DIMENSION"))
                            .child(div().flex_1().min_w_0().child("VALUE"))
                            .child(div().w(px(72.)).child("N"))
                            .child(div().w(px(82.)).child("F1"))
                            .child(div().w(px(82.)).child("MCC"))
                            .child(div().w(px(82.)).child("BRIER")),
                    )
                    .children(slices.iter().map(|slice| {
                        div()
                            .py_2()
                            .border_b_1()
                            .border_color(Theme::BORDER)
                            .flex()
                            .items_center()
                            .gap_3()
                            .text_sm()
                            .child(div().w(px(110.)).child(slice.dimension.clone()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_ellipsis()
                                    .child(slice.value.clone()),
                            )
                            .child(
                                div()
                                    .w(px(72.))
                                    .child(slice.report.attempted_count.to_string()),
                            )
                            .child(div().w(px(82.)).child(format_optional(slice.report.f1)))
                            .child(
                                div()
                                    .w(px(82.))
                                    .child(format_optional(slice.report.matthews_correlation)),
                            )
                            .child(
                                div()
                                    .w(px(82.))
                                    .child(format_optional(slice.report.brier_score)),
                            )
                    }))
            })
    }

    fn render_threshold_policy(&self, cx: &mut Context<Self>) -> Div {
        let automation_ready = self
            .selected_release()
            .zip(self.reports.first())
            .is_some_and(|((_, release), held_out)| {
                let fit_random = random_audit_report(&release.fit_slice_reports);
                let test_random = random_audit_report(&held_out.slice_reports);
                fit_random.zip(test_random).is_some_and(|(fit, test)| {
                    automation_population_sufficient(fit, test)
                        && automation_quality_sufficient(test)
                })
            });
        div().mt_8().pt_6().border_t_1().border_color(Theme::BORDER)
            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child("Threshold-policy release"))
            .child(div().mt_1().text_sm().text_color(Theme::MUTED)
                .child("Create a new immutable decision policy. The evaluator release and prior decisions remain unchanged."))
            .child(div().mt_4().flex().flex_wrap().gap_3()
                .child(threshold_field("Auto-pass below", self.pass_threshold.clone()))
                .child(threshold_field("Review / fail at", self.fail_threshold.clone()))
                .child(threshold_field("Minimum confidence", self.minimum_confidence.clone())))
            .when_some(self.error.clone(), |section, error| section.child(div().mt_3().p_3().rounded_sm().bg(Theme::DANGER_SURFACE).text_sm().text_color(Theme::RED).child(error)))
            .when_some(self.notice.clone(), |section, notice| section.child(div().mt_3().p_3().rounded_sm().bg(Theme::SUCCESS_SURFACE).text_sm().text_color(Theme::GREEN).child(notice)))
            .when(self.held_out_report().is_some() && !automation_ready, |section| section.child(
                div().id("automation-safety-gate-warning").mt_3().p_3().rounded_sm().bg(Theme::WARNING_SURFACE).text_sm().text_color(Theme::AMBER)
                    .role(Role::Alert)
                    .aria_label("Automation activation is blocked. The safety-gate checklist above shows current values and failed requirements.")
                    .child("Exploratory report only. Activation requires an unbiased random-audit cohort with at least 500 resolved labels, 100 examples in each class, 90% held-out decision coverage, and a grouped-confidence bound above the baselines."),
            ))
            .child(div().mt_4().flex().gap_2()
                .child(button_state("1 · Freeze threshold policy", true, !self.busy && self.proposed_policy.is_none() && self.held_out_report().is_none() && self.selected_release().is_some_and(|(_, release)| agreement_is_sufficient(release)))
                    .id("create-threshold-policy")
                    .role(Role::Button)
                    .aria_label("Step 1: freeze an immutable threshold policy")
                    .on_click(cx.listener(|this, _, _, cx| this.create_policy(cx))))
                .when(self.proposed_policy.is_some() && self.held_out_report().is_none(), |actions| actions.child(
                    button_state("2 · Evaluate held-out test once", false, !self.busy)
                        .id("evaluate-held-out")
                        .role(Role::Button)
                        .aria_label("Step 2: evaluate the held-out test exactly once")
                        .on_click(cx.listener(|this, _, _, cx| this.evaluate_held_out(cx)))
                ))
                .when(self.proposed_policy.is_some() && self.held_out_report().is_some(), |actions| actions.child(
                    button_state("2 · Held-out test evaluated", true, false)
                        .id("held-out-evaluated")
                        .role(Role::Status)
                        .aria_label("Step 2 complete: the held-out test was evaluated exactly once")
                ).child(
                    button_state("3 · Activate reviewed policy", false, !self.busy && automation_ready)
                        .id("activate-threshold-policy")
                        .role(Role::Button)
                        .aria_label(if automation_ready { "Step 3: activate the reviewed automation policy" } else { "Step 3 blocked: automation safety gates have not passed" })
                        .when(automation_ready, |button| button.on_click(cx.listener(|this, _, _, cx| this.activate_policy(cx))))
                )))
    }
}

fn agreement_is_sufficient(release: &CalibrationReleaseV1) -> bool {
    matches!(
        release.agreement_report.krippendorff_alpha,
        Some(alpha) if alpha.is_finite() && alpha >= 0.67
    )
}

fn random_audit_report(
    slices: &[CalibrationSliceReportV1],
) -> Option<&perseval_service::BinaryCalibrationReportV1> {
    slices
        .iter()
        .find(|slice| slice.dimension == "selection stream" && slice.value == "random audit")
        .map(|slice| &slice.report)
}

fn automation_population_sufficient(
    fit: &perseval_service::BinaryCalibrationReportV1,
    held_out: &perseval_service::BinaryCalibrationReportV1,
) -> bool {
    let label_count = fit.attempted_count.saturating_add(held_out.attempted_count);
    let positive_count = fit
        .confusion
        .true_positive
        .saturating_add(fit.confusion.false_negative)
        .saturating_add(held_out.confusion.true_positive)
        .saturating_add(held_out.confusion.false_negative);
    let negative_count = fit
        .confusion
        .true_negative
        .saturating_add(fit.confusion.false_positive)
        .saturating_add(held_out.confusion.true_negative)
        .saturating_add(held_out.confusion.false_positive);
    label_count >= 500 && positive_count >= 100 && negative_count >= 100
}

fn automation_quality_metrics_sufficient(
    report: &perseval_service::BinaryCalibrationReportV1,
) -> bool {
    report.average_precision.is_some_and(|value| value >= 0.65)
        && report.macro_f1.is_some_and(|value| value >= 0.60)
        && report.precision.is_some_and(|value| value >= 0.60)
        && report.recall.is_some_and(|value| value >= 0.60)
        && report.f1.is_some_and(|value| value > 0.206)
        && report
            .matthews_correlation
            .is_some_and(|value| value > 0.200)
        && report.brier_score.is_some_and(|value| value <= 0.20)
        && report
            .expected_calibration_error
            .is_some_and(|value| value <= 0.08)
}

fn automation_quality_sufficient(report: &perseval_service::BinaryCalibrationReportV1) -> bool {
    let decision_coverage = if report.attempted_count == 0 {
        0.0
    } else {
        report.decided_count as f64 / report.attempted_count as f64
    };
    automation_quality_metrics_sufficient(report)
        && decision_coverage >= 0.90
        && report
            .macro_f1_interval
            .as_ref()
            .is_some_and(|interval| interval.lower_95 > 0.053)
}

impl Render for CalibrationScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Theme::BG)
            .text_color(Theme::TEXT)
            .child(
                div()
                    .h(px(58.))
                    .flex_none()
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(Theme::BORDER)
                    .bg(Theme::PANEL)
                    .child(
                        div()
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Calibration"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(Theme::MUTED)
                                    .child("Compare learned judges with independent human answers"),
                            ),
                    )
                    .child(
                        button_state("Refresh", false, !self.busy)
                            .id("refresh-calibration")
                            .role(Role::Button)
                            .aria_label("Refresh calibration releases and reports")
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_release_list(cx))
                    .child(self.render_metrics(cx)),
            )
    }
}

fn input(
    value: &str,
    placeholder: &'static str,
    cx: &mut Context<CalibrationScreen>,
) -> Entity<TextInput> {
    let value = value.to_owned();
    cx.new(|cx| {
        let mut input = TextInput::new(placeholder, 16, cx);
        input.set_text(value, cx);
        input
    })
}

fn threshold_field(label: &str, input: Entity<TextInput>) -> Div {
    div()
        .w(px(170.))
        .child(
            div()
                .text_xs()
                .text_color(Theme::MUTED)
                .child(label.to_owned()),
        )
        .child(div().mt_1().child(input))
}

fn metric(label: &str, value: String) -> Div {
    div()
        .min_w(px(138.))
        .p_3()
        .rounded_sm()
        .border_1()
        .border_color(Theme::BORDER)
        .bg(Theme::PANEL_SURFACE)
        .child(
            div()
                .text_xs()
                .text_color(Theme::MUTED)
                .child(label.to_owned()),
        )
        .child(
            div()
                .mt_1()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
}

fn metric_heading(label: &str, value: &str) -> Div {
    div()
        .child(
            div()
                .text_xs()
                .text_color(Theme::MUTED)
                .child(label.to_owned()),
        )
        .child(
            div()
                .mt_1()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(value.to_owned()),
        )
}

fn metric_empty(message: &str) -> Div {
    div()
        .p_4()
        .rounded_sm()
        .border_1()
        .border_color(Theme::BORDER)
        .text_sm()
        .text_color(Theme::MUTED)
        .child(message.to_owned())
}

fn format_optional(value: Option<f64>) -> String {
    value.map_or_else(|| "not available".into(), |value| format!("{value:.3}"))
}

fn format_rate_interval(estimate: Option<f64>, interval: Option<(f64, f64, u64, u64)>) -> String {
    match (estimate, interval) {
        (Some(estimate), Some((lower, upper, successes, trials))) => {
            format!("{estimate:.3} · {lower:.3}–{upper:.3} · {successes}/{trials}")
        }
        _ => "not available".into(),
    }
}

fn short_id(value: &str) -> &str {
    value.get(..value.len().min(12)).unwrap_or(value)
}

fn metric_view_ordinal(view: MetricView) -> u32 {
    match view {
        MetricView::Confusion => 0,
        MetricView::Agreement => 1,
        MetricView::Calibration => 2,
        MetricView::RiskCoverage => 3,
    }
}
