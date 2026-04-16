use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{auth::access::AccessControl, user::Role},
};
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use sqlx::migrate;
use tracing::{error, info};

#[utoipa::path(
    get,
    path = "/api/v1/migrations",
    tags = ["Migrations"],
    summary = "Dry run database migrations.",
    description = "Simulates database migrations. Currently not implemented.",
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 501, description = "Not Implemented")
    )
)]
pub async fn dry_run(access: AccessControl) -> Result<impl IntoResponse, ApiError> {
    access.require_role(Role::Admin)?;

    Ok((
        StatusCode::NOT_IMPLEMENTED,
        Json("Dry run mode is planned but has not been implemented yet."),
    ))
}

/// Executes pending database migrations.
///
/// This endpoint allows users to apply any pending database migrations.
/// It checks for migrations that need to be applied and executes them.
/// If the migrations are applied successfully, a confirmation message is returned.
#[utoipa::path(
    post,
    path = "/api/v1/migrations",
    tags = ["Migrations"],
    summary = "Execute pending database migrations.",
    description = "This endpoint executes any pending migrations in the database. It applies migrations that have not yet been run and provides confirmation upon success.",
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Migrations applied successfully", body = String),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 500, description = "An error occurred while applying migrations")
    )
)]
pub async fn live_run(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_role(Role::Admin)?;

    migrate!("./src/database/migrations")
        .run(&state.db)
        .await
        .map_err(|e| {
            error!("Error applying migrations: {e}");
            ApiError::DatabaseError(e.into())
        })?;

    info!("Migrations applied successfully!");
    Ok(Json("Migrations applied successfully!"))
}
