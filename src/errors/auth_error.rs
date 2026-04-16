#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Authorization token is missing in the request. Please provide a valid JWT token.")]
    MissingToken,
    #[error("Invalid JWT token. Please provide a valid token.")]
    InvalidToken,
}
