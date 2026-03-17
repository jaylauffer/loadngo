use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProactorError {
    #[error("completion port closed")]
    Closed,
}
