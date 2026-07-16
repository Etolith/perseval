/// Transactional state such as cursors, jobs, reviews, and artifact identities.
pub trait ControlStore: Send + Sync {
    fn backend_name(&self) -> &'static str;
}
