use perseval_service::AnalysisStatus;

use super::*;

fn run(id: &str, project_id: &str, lifecycle: TraceLifecycle) -> RunSummary {
    RunSummary {
        project_id: project_id.into(),
        logical_trace_id: id.into(),
        external_trace_id: id.into(),
        revision: 1,
        lifecycle,
        title: id.into(),
        service_name: None,
        environment: None,
        session_id: None,
        build_id: None,
        agent_id: None,
        identity_quality: IdentityQualityV1::Unknown,
        start_time_unix_nano: 0,
        end_time_unix_nano: 1,
        last_committed_unix_ms: 0,
        span_count: 1,
        error_count: 0,
        analysis_status: AnalysisStatus::Ready,
        finding_count: 0,
    }
}

#[test]
fn comparison_requires_exactly_two_finalized_runs_in_one_project() {
    let baseline = run("baseline", "checkout", TraceLifecycle::Finalized);
    let candidate = run("candidate", "checkout", TraceLifecycle::Finalized);
    let scope = QueryScopeV1::new(QueryScopeCriteriaV1 {
        project_id: Some("checkout".into()),
        ..QueryScopeCriteriaV1::default()
    });
    let request = comparison_request(&scope, &[baseline, candidate]).unwrap();

    assert_eq!(
        request.scope.criteria.project_id.as_deref(),
        Some("checkout")
    );
    assert_eq!(request.baseline_trace_id, "baseline");
    assert_eq!(request.candidate_trace_id, "candidate");
}

#[test]
fn comparison_rejects_all_projects_cross_project_and_live_runs() {
    let finalized = run("baseline", "checkout", TraceLifecycle::Finalized);
    assert!(
        comparison_request(
            &QueryScopeV1::default(),
            &[finalized.clone(), finalized.clone()]
        )
        .is_err()
    );
    let scope = QueryScopeV1::new(QueryScopeCriteriaV1 {
        project_id: Some("checkout".into()),
        ..QueryScopeCriteriaV1::default()
    });
    assert!(
        comparison_request(
            &scope,
            &[
                finalized.clone(),
                run("other", "support", TraceLifecycle::Finalized),
            ],
        )
        .is_err()
    );
    assert!(
        comparison_request(
            &scope,
            &[finalized, run("live", "checkout", TraceLifecycle::Live)],
        )
        .is_err()
    );
}

#[test]
fn time_window_options_are_bounded_and_explicit() {
    let windows = [
        RunTimeWindow::All,
        RunTimeWindow::LastHour,
        RunTimeWindow::LastDay,
        RunTimeWindow::LastWeek,
    ];
    assert_eq!(windows.len(), 4);
    assert_eq!(windows[0].duration_nanos(), None);
    assert_eq!(windows[1].duration_nanos(), Some(3_600 * NANOS_PER_SECOND));
    assert_eq!(windows[3].label(), "Last 7 days");
}
