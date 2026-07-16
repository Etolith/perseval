use serde_json::{Value, json};

/// A small deterministic OTLP data set for local product evaluation. It uses
/// ordinary OTel/OpenInference attributes and enters through the production
/// decoder, journal, projection, and analysis path.
pub(crate) fn local_demo_otlp_json() -> Vec<u8> {
    let mut resource_spans = Vec::new();
    let base_time = 1_767_225_600_000_000_000_u64;
    for run in 1_u8..=3 {
        let mut spans = Vec::new();
        let trace_id = format!("{run:02x}").repeat(16);
        let root_seed = run.saturating_mul(32);
        let root_id = format!("{root_seed:02x}").repeat(8);
        let run_start = base_time + run as u64 * 86_400_000_000_000;
        spans.push(json!({
            "traceId": trace_id,
            "spanId": root_id,
            "name": "agent.run · checkout-agent",
            "kind": 1,
            "startTimeUnixNano": run_start.to_string(),
            "endTimeUnixNano": (run_start + 4_820_000_000).to_string(),
            "attributes": [
                attribute("openinference.span.kind", "AGENT"),
                attribute("gen_ai.operation.name", "invoke_agent"),
                attribute("agent.name", "checkout-agent"),
                attribute("gen_ai.conversation.id", &format!("demo-session-{run}")),
                attribute("agent.final.status", "failed"),
            ],
            "status": { "code": 2, "message": "checkout remained unresolved" }
        }));

        let planner_id = format!("{:02x}", root_seed + 1).repeat(8);
        spans.push(json!({
            "traceId": trace_id,
            "spanId": planner_id,
            "parentSpanId": root_id,
            "name": "planner.run",
            "kind": 1,
            "startTimeUnixNano": (run_start + 100_000_000).to_string(),
            "endTimeUnixNano": (run_start + 1_220_000_000).to_string(),
            "attributes": [
                attribute("openinference.span.kind", "AGENT"),
                attribute("agent.name", "planner"),
                attribute("gen_ai.operation.name", "plan"),
                attribute("gen_ai.input.messages", "Find an in-stock item, add it to checkout, and verify the order state."),
            ],
            "status": { "code": 1 }
        }));
        spans.push(json!({
            "traceId": trace_id,
            "spanId": format!("{:02x}", root_seed + 2).repeat(8),
            "parentSpanId": planner_id,
            "name": "model.response · gpt-5.2",
            "kind": 1,
            "startTimeUnixNano": (run_start + 220_000_000).to_string(),
            "endTimeUnixNano": (run_start + 318_000_000).to_string(),
            "attributes": [
                attribute("openinference.span.kind", "LLM"),
                attribute("agent.name", "planner"),
                attribute("gen_ai.request.model", "gpt-5.2"),
                attribute("gen_ai.operation.name", "chat"),
            ],
            "status": { "code": 1 }
        }));

        let browser_id = format!("{:02x}", root_seed + 3).repeat(8);
        spans.push(json!({
            "traceId": trace_id,
            "spanId": browser_id,
            "parentSpanId": root_id,
            "name": "browser.agent",
            "kind": 1,
            "startTimeUnixNano": (run_start + 1_300_000_000).to_string(),
            "endTimeUnixNano": (run_start + 4_240_000_000).to_string(),
            "attributes": [
                attribute("openinference.span.kind", "AGENT"),
                attribute("agent.name", "browser"),
                attribute("gen_ai.operation.name", "invoke_agent"),
            ],
            "status": { "code": 2, "message": "repeated click produced no state transition" }
        }));
        for attempt in 1_u8..=3 {
            let click_id = format!("{:02x}", root_seed + 2 * attempt + 2).repeat(8);
            let start = run_start + 1_350_000_000 + attempt as u64 * 700_000_000;
            spans.push(json!({
                "traceId": trace_id,
                "spanId": click_id,
                "parentSpanId": browser_id,
                "name": "browser.click · checkout button",
                "kind": 3,
                "startTimeUnixNano": start.to_string(),
                "endTimeUnixNano": (start + 76_000_000).to_string(),
                "attributes": [
                    attribute("openinference.span.kind", "TOOL"),
                    attribute("gen_ai.tool.name", "browser.click"),
                    attribute("agent.name", "browser"),
                    attribute("agent.operation", "click_checkout"),
                    attribute("agent.tool.attempt", &attempt.to_string()),
                    attribute("agent.tool.requirement", "required"),
                    bool_attribute("tool.result.success", false),
                    attribute("tool.error.code", "state_unchanged"),
                    attribute("tool.error.message", "checkout button click produced no state transition"),
                    attribute("gen_ai.tool.call.arguments", "{\"selector\":\"[data-test=checkout]\",\"button\":\"left\"}"),
                ],
                "status": { "code": 2, "message": "page state remained unchanged" }
            }));
            spans.push(json!({
                "traceId": trace_id,
                "spanId": format!("{:02x}", root_seed + 2 * attempt + 3).repeat(8),
                "parentSpanId": browser_id,
                "name": "page.snapshot · identical state",
                "kind": 1,
                "startTimeUnixNano": (start + 90_000_000).to_string(),
                "endTimeUnixNano": (start + 128_000_000).to_string(),
                "attributes": [
                    attribute("openinference.span.kind", "CHAIN"),
                    attribute("agent.name", "browser"),
                    attribute("state.observation", "verified_unchanged"),
                    attribute("browser.dom.fingerprint", "sha256:demo-checkout-unchanged"),
                ],
                "status": { "code": 1 }
            }));
        }

        spans.push(json!({
            "traceId": trace_id,
            "spanId": format!("{:02x}", root_seed + 11).repeat(8),
            "parentSpanId": root_id,
            "name": "verifier.agent",
            "kind": 1,
            "startTimeUnixNano": (run_start + 4_160_000_000).to_string(),
            "endTimeUnixNano": (run_start + 4_802_000_000).to_string(),
            "attributes": [
                attribute("openinference.span.kind", "AGENT"),
                attribute("agent.name", "verifier"),
                attribute("gen_ai.operation.name", "verify"),
                bool_attribute("verifier.objective.resolved", false),
            ],
            "status": { "code": 1 }
        }));

        resource_spans.push(json!({
            "resource": { "attributes": [
                attribute("service.name", "checkout-agent"),
                attribute("service.version", &format!("v0.18.{}", run + 1)),
                attribute("deployment.environment.name", "local-demo"),
                bool_attribute("perseval.sample", true),
            ]},
            "scopeSpans": [{
                "scope": { "name": "perseval.local-demo", "version": "2" },
                "spans": spans
            }]
        }));
    }

    serde_json::to_vec(&json!({
        "resourceSpans": resource_spans
    }))
    .expect("static demo OTLP JSON is serializable")
}

fn attribute(key: &str, value: &str) -> Value {
    json!({ "key": key, "value": { "stringValue": value } })
}

fn bool_attribute(key: &str, value: bool) -> Value {
    json!({ "key": key, "value": { "boolValue": value } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_payload_has_three_multi_agent_runs() {
        let value: Value = serde_json::from_slice(&local_demo_otlp_json()).unwrap();
        let resource_spans = value["resourceSpans"].as_array().unwrap();
        let spans = resource_spans
            .iter()
            .flat_map(|resource| resource["scopeSpans"][0]["spans"].as_array().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(spans.len(), 33);
        assert_eq!(
            spans
                .iter()
                .map(|span| span["traceId"].as_str().unwrap())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3
        );
        for expected in ["planner.run", "browser.agent", "verifier.agent"] {
            assert!(spans.iter().any(|span| span["name"] == expected));
        }
        let attributes = spans
            .iter()
            .flat_map(|span| span["attributes"].as_array().into_iter().flatten())
            .filter_map(|attribute| attribute["key"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            attributes
                .iter()
                .filter(|key| **key == "agent.final.status")
                .count(),
            3
        );
        assert_eq!(
            attributes
                .iter()
                .filter(|key| **key == "agent.tool.requirement")
                .count(),
            9
        );
    }
}
