use crate::openapi::api_doc::ApiDoc;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

/// The Swagger UI and the OpenAPI document. Served only when
/// `ENABLE_SWAGGER` is on (see `routes::create_routes`).
pub fn swagger_routes() -> SwaggerUi {
    SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi())
}
