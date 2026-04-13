#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Authorization token is missing in the request. Please provide a valid JWT token.")]
    MissingToken,
    #[error("Authorization header cannot be empty. Please provide a valid JWT token.")]
    EmptyHeader,
    #[error("Invalid JWT token. Please provide a valid token.")]
    InvalidToken,
}
