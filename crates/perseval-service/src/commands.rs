#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub artifact_id: String,
}

pub trait CommandService: Send + Sync {
    type Error;

    fn execute(&self, request: CommandRequest) -> Result<CommandOutcome, Self::Error>;
}
