use super::*;

impl LiveTraceService {
    pub fn import_otlp_file(
        &self,
        project_id: &str,
        path: &Path,
    ) -> Result<TraceFileImportResultV1, LiveServiceError> {
        let project_id = project_id.trim();
        let (content_type, content_encoding) = import_wire_format(path)?;
        let metadata = fs::metadata(path).map_err(|error| {
            LiveServiceError::InvalidImport(format!("could not read file metadata: {error}"))
        })?;
        if !metadata.is_file() {
            return Err(LiveServiceError::InvalidImport(
                "the selected path is not a file".into(),
            ));
        }
        if metadata.len() > self.config.otlp.max_wire_bytes as u64 {
            return Err(LiveServiceError::InvalidImport(format!(
                "file exceeds the {} MiB wire-size limit",
                self.config.otlp.max_wire_bytes / (1024 * 1024)
            )));
        }
        let raw = fs::read(path).map_err(|error| {
            LiveServiceError::InvalidImport(format!("could not read trace file: {error}"))
        })?;
        self.import_otlp_payload(
            project_id,
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("trace file"),
            raw,
            content_type,
            content_encoding,
            "file",
        )
    }

    pub fn load_local_demo(
        &self,
        project_id: &str,
    ) -> Result<TraceFileImportResultV1, LiveServiceError> {
        self.import_otlp_payload(
            project_id,
            "Perseval local demo",
            crate::demo_trace::local_demo_otlp_json(),
            "application/json",
            "identity",
            "sample",
        )
    }

    fn import_otlp_payload(
        &self,
        project_id: &str,
        display_name: &str,
        raw: Vec<u8>,
        content_type: &str,
        content_encoding: &str,
        source_kind: &str,
    ) -> Result<TraceFileImportResultV1, LiveServiceError> {
        let project_id = project_id.trim();
        if !self
            .store
            .list_projects()?
            .iter()
            .any(|project| project.project_id == project_id)
        {
            return Err(LiveServiceError::InvalidImport(format!(
                "select an existing project before importing (unknown project '{project_id}')"
            )));
        }
        let receiver_config = OtlpReceiverConfig {
            enabled: self.config.otlp.enabled,
            bind_addr: self.config.otlp.bind_addr,
            source_id: self.config.otlp.source_id.clone(),
            max_wire_bytes: self.config.otlp.max_wire_bytes,
            max_decoded_bytes: self.config.otlp.max_decoded_bytes,
            max_spans_per_request: self.config.otlp.max_spans_per_request,
            max_attributes_per_span: self.config.otlp.max_attributes_per_span,
            retry_after_seconds: self.config.otlp.retry_after_seconds,
        };
        let mut submission =
            prepare_otlp_submission(&receiver_config, raw, content_type, content_encoding)
                .map_err(LiveServiceError::InvalidImport)?;
        let file_source_id = format!("{}:{source_kind}:{project_id}", self.config.otlp.source_id);
        submission.batch.source_id.clone_from(&file_source_id);
        for span in &mut submission.batch.spans {
            span.source_id.clone_from(&file_source_id);
            span.logical_trace_id = format!(
                "{:x}",
                Sha256::digest(format!("{file_source_id}:{}", span.external_trace_id))
            );
            span.resource.insert(
                "perseval.project.id".into(),
                serde_json::Value::String(project_id.into()),
            );
        }
        let accepted_spans = submission.batch.spans.len() as u64;
        let rejected_spans = submission.batch.rejected_spans;
        let admission = self
            .ingest
            .submit_blocking(submission)
            .map_err(import_submit_error)?;
        Ok(TraceFileImportResultV1 {
            file_name: display_name.into(),
            project_id: project_id.into(),
            accepted_spans,
            rejected_spans,
            duplicate_request: admission.duplicate_request,
        })
    }
}

fn import_wire_format(path: &Path) -> Result<(&'static str, &'static str), LiveServiceError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".json.gz") {
        Ok(("application/json", "gzip"))
    } else if name.ends_with(".pb.gz")
        || name.ends_with(".protobuf.gz")
        || name.ends_with(".bin.gz")
    {
        Ok(("application/x-protobuf", "gzip"))
    } else if name.ends_with(".json") {
        Ok(("application/json", "identity"))
    } else if name.ends_with(".pb") || name.ends_with(".protobuf") || name.ends_with(".bin") {
        Ok(("application/x-protobuf", "identity"))
    } else {
        Err(LiveServiceError::InvalidImport(
            "supported trace files end in .json, .pb, .protobuf, .bin, or a gzip-compressed form"
                .into(),
        ))
    }
}

fn import_submit_error(error: OtlpSubmitError) -> LiveServiceError {
    match error {
        OtlpSubmitError::Backpressured => LiveServiceError::InvalidImport(
            "the bounded ingest queue is full; retry after current projection work drains".into(),
        ),
        OtlpSubmitError::ShuttingDown => LiveServiceError::WriterUnavailable,
        OtlpSubmitError::Unavailable(error) => LiveServiceError::Writer(error),
    }
}
