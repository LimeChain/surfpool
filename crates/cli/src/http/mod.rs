#![allow(unused_imports, unused_variables)]
use std::{
    collections::HashMap,
    error::Error as StdError,
    str::FromStr,
    sync::{Arc, RwLock},
    thread::JoinHandle,
    time::Duration,
};

use actix_cors::Cors;
use actix_web::{
    App, Error, HttpRequest, HttpResponse, HttpServer, Responder,
    dev::ServerHandle,
    http::header::{self},
    middleware, post,
    web::{self, Data, route},
};
use convert_case::{Case, Casing};
use crossbeam::channel::{Receiver, Select, Sender};
use juniper_actix::{graphiql_handler, graphql_handler, subscriptions};
use juniper_graphql_ws::ConnectionConfig;
use log::{debug, error, info, trace, warn};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp_actix_web::transport::StreamableHttpService;
#[cfg(feature = "explorer")]
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use surfpool_core::scenarios::TemplateRegistry;
use surfpool_mcp::Surfpool;
use surfpool_studio_ui::serve_studio_static_files;
use surfpool_types::{
    DataIndexingCommand, OverrideTemplate, SanitizedConfig, Scenario, SubgraphEvent, SurfpoolConfig,
};
use txtx_core::kit::types::types::Value;
use txtx_gql::kit::uuid::Uuid;

use crate::cli::Context;

#[cfg(feature = "explorer")]
#[derive(RustEmbed)]
#[folder = "../../../explorer/.next/server/app"]
pub struct Asset;

/// Registers the studio API routes. Shared between the server and its tests
fn configure_api(cfg: &mut web::ServiceConfig) {
    cfg.service(get_config)
        .service(get_scenario_templates)
        .service(post_scenarios)
        .service(get_scenarios)
        .service(delete_scenario)
        .service(patch_scenario)
        // Unknown /v1/* paths must fail loudly here: otherwise the studio
        // SPA fallback answers them with index.html and a misleading 200
        .service(web::scope("/v1").default_service(web::route().to(api_not_found)));
}

pub async fn start_studio_and_scenario_server(
    network_binding: String,
    config: SanitizedConfig,
    subgraph_events_tx: Sender<SubgraphEvent>,
    ctx: &Context,
    enable_studio: bool,
) -> Result<ServerHandle, Box<dyn StdError>> {
    let config_wrapped = Data::new(RwLock::new(config.clone()));
    // Initialize template registry and load templates
    let template_registry_wrapped = Data::new(RwLock::new(TemplateRegistry::new()));
    let loaded_scenarios = Data::new(RwLock::new(LoadedScenarios::new()));

    // Initialize MCP service
    let mcp_service = StreamableHttpService::builder()
        .service_factory(Arc::new(|| Ok(Surfpool::new())))
        .session_manager(Arc::new(LocalSessionManager::default()))
        .stateful_mode(true)
        .sse_keep_alive(Duration::from_secs(30))
        .build();

    let server = HttpServer::new(move || {
        let mut app = App::new()
            .app_data(config_wrapped.clone())
            .app_data(template_registry_wrapped.clone())
            .app_data(loaded_scenarios.clone())
            .wrap(
                Cors::default()
                    .allow_any_origin()
                    .allow_any_method()
                    .allow_any_header()
                    .expose_headers(vec!["Mcp-Session-Id", "mcp-session-id"])
                    .supports_credentials()
                    .max_age(3600),
            )
            .wrap(middleware::Compress::default())
            .wrap(middleware::Logger::default())
            .configure(configure_api)
            .service(web::scope("/mcp").service(mcp_service.clone().scope()));

        if enable_studio {
            app = app.app_data(Arc::new(RwLock::new(LoadedScenarios::new())));
            app = app.service(serve_studio_static_files);
        }

        app
    })
    .workers(5)
    .bind(network_binding)?
    .run();
    let handle = server.handle();
    tokio::spawn(server);
    Ok(handle)
}

#[cfg(feature = "explorer")]
fn handle_embedded_file(path: &str) -> HttpResponse {
    use mime_guess::from_path;
    match Asset::get(path) {
        Some(content) => HttpResponse::Ok()
            .content_type(from_path(path).first_or_octet_stream().as_ref())
            .body(content.data.into_owned()),
        None => {
            if let Some(index_content) = Asset::get("index.html") {
                HttpResponse::Ok()
                    .content_type("text/html")
                    .body(index_content.data.into_owned())
            } else {
                HttpResponse::NotFound().body("404 Not Found")
            }
        }
    }
}

#[actix_web::get("/config")]
async fn get_config(
    req: HttpRequest,
    payload: web::Payload,
    config: Data<RwLock<SanitizedConfig>>,
) -> Result<HttpResponse, Error> {
    let config = config
        .read()
        .map_err(|_| actix_web::error::ErrorInternalServerError("Failed to read context"))?;
    let api_config = serde_json::json!(*config);
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(api_config.to_string()))
}

#[actix_web::get("/v1/scenarios/templates")]
async fn get_scenario_templates(
    template_registry: Data<RwLock<TemplateRegistry>>,
) -> Result<HttpResponse, Error> {
    let registry = template_registry.read().map_err(|_| {
        actix_web::error::ErrorInternalServerError("Failed to read template registry")
    })?;

    let templates: Vec<&OverrideTemplate> = registry.all();
    let response = serde_json::to_string(&templates)
        .map_err(|_| actix_web::error::ErrorInternalServerError("Failed to serialize templates"))?;

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(response))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoadedScenarios {
    pub scenarios: Vec<Scenario>,
}
impl LoadedScenarios {
    pub fn new() -> Self {
        Self {
            scenarios: Vec::new(),
        }
    }
}

#[post("/v1/scenarios")]
async fn post_scenarios(
    scenario: web::Json<Scenario>,
    data: Data<RwLock<LoadedScenarios>>,
) -> Result<HttpResponse, Error> {
    let scenario_data = scenario.into_inner();

    let mut loaded_scenarios = data
        .write()
        .map_err(|_| actix_web::error::ErrorInternalServerError("Failed to acquire write lock"))?;
    let scenario_id = scenario_data.id.clone();

    if let Some(existing) = loaded_scenarios
        .scenarios
        .iter()
        .find(|s| s.id == scenario_id)
    {
        let identical = match (
            serde_json::to_value(existing),
            serde_json::to_value(&scenario_data),
        ) {
            (Ok(stored), Ok(incoming)) => stored == incoming,
            _ => false,
        };

        if identical {
            let response = serde_json::json!({"id": scenario_id});
            return Ok(HttpResponse::Ok()
                .content_type("application/json")
                .body(response.to_string()));
        }

        let response = serde_json::json!({
            "error": "a different scenario is already stored under this id",
            "id": scenario_id,
        });
        return Ok(HttpResponse::Conflict()
            .content_type("application/json")
            .body(response.to_string()));
    }

    loaded_scenarios.scenarios.push(scenario_data);
    let response = serde_json::json!({"id": scenario_id});
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(response.to_string()))
}

#[actix_web::get("/v1/scenarios")]
async fn get_scenarios(data: Data<RwLock<LoadedScenarios>>) -> Result<HttpResponse, Error> {
    let loaded_scenarios = data
        .read()
        .map_err(|_| actix_web::error::ErrorInternalServerError("Failed to acquire read lock"))?;
    let response = serde_json::to_string(&loaded_scenarios.scenarios).map_err(|_| {
        actix_web::error::ErrorInternalServerError("Failed to serialize loaded scenarios")
    })?;

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(response))
}

#[actix_web::delete("/v1/scenarios/{id}")]
async fn delete_scenario(
    path: web::Path<String>,
    data: Data<RwLock<LoadedScenarios>>,
) -> Result<HttpResponse, Error> {
    let scenario_id = path.into_inner();
    let mut loaded_scenarios = data
        .write()
        .map_err(|_| actix_web::error::ErrorInternalServerError("Failed to acquire write lock"))?;

    let initial_len = loaded_scenarios.scenarios.len();
    loaded_scenarios.scenarios.retain(|s| s.id != scenario_id);

    if loaded_scenarios.scenarios.len() == initial_len {
        return Ok(
            HttpResponse::NotFound().body(format!("Scenario with id '{}' not found", scenario_id))
        );
    }

    Ok(HttpResponse::Ok().body(format!("Scenario '{}' deleted", scenario_id)))
}

#[actix_web::patch("/v1/scenarios/{id}")]
async fn patch_scenario(
    path: web::Path<String>,
    scenario: web::Json<Scenario>,
    data: Data<RwLock<LoadedScenarios>>,
) -> Result<HttpResponse, Error> {
    let scenario_id = path.into_inner();
    let mut loaded_scenarios = data
        .write()
        .map_err(|_| actix_web::error::ErrorInternalServerError("Failed to acquire write lock"))?;

    let scenario_index = loaded_scenarios
        .scenarios
        .iter()
        .position(|s| s.id == scenario_id);

    match scenario_index {
        Some(index) => {
            loaded_scenarios.scenarios[index] = scenario.into_inner();
            let response = serde_json::json!({"id": scenario_id});
            Ok(HttpResponse::Ok()
                .content_type("application/json")
                .body(response.to_string()))
        }
        None => {
            loaded_scenarios.scenarios.push(scenario.into_inner());
            let response = serde_json::json!({"id": scenario_id});
            Ok(HttpResponse::Ok()
                .content_type("application/json")
                .body(response.to_string()))
        }
    }
}

#[allow(dead_code)]
#[cfg(not(feature = "explorer"))]
fn handle_embedded_file(_path: &str) -> HttpResponse {
    HttpResponse::NotFound().body("404 Not Found")
}

#[actix_web::get("/{_:.*}")]
async fn dist(path: web::Path<String>) -> impl Responder {
    let path_str = match path.as_str() {
        "" => "index.html",
        other => other,
    };
    handle_embedded_file(path_str)
}

async fn api_not_found() -> HttpResponse {
    HttpResponse::NotFound()
        .content_type("application/json")
        .body(r#"{"error":"not found"}"#)
}

#[cfg(test)]
mod tests {
    use actix_web::{App, test};

    use super::*;

    fn scenario_body() -> serde_json::Value {
        serde_json::json!({
            "id": "s1",
            "name": "first",
            "description": "",
            "overrides": [],
            "tags": [],
        })
    }

    fn post_scenario(body: serde_json::Value) -> test::TestRequest {
        test::TestRequest::post()
            .uri("/v1/scenarios")
            .set_json(body)
    }

    #[actix_web::test]
    async fn creating_the_same_scenario_twice_is_a_no_op() {
        let loaded_scenarios = Data::new(RwLock::new(LoadedScenarios::new()));
        let app = test::init_service(
            App::new()
                .app_data(loaded_scenarios.clone())
                .configure(configure_api),
        )
        .await;

        let created = test::call_service(&app, post_scenario(scenario_body()).to_request()).await;
        assert_eq!(created.status(), 200, "first create must succeed");

        let retried = test::call_service(&app, post_scenario(scenario_body()).to_request()).await;
        assert_eq!(
            retried.status(),
            200,
            "an identical retry must be accepted as a no-op"
        );

        let stored = &loaded_scenarios.read().unwrap().scenarios;
        assert_eq!(stored.len(), 1, "no duplicate may be stored");
    }

    #[actix_web::test]
    async fn reusing_a_scenario_id_for_different_content_conflicts() {
        let loaded_scenarios = Data::new(RwLock::new(LoadedScenarios::new()));
        let app = test::init_service(
            App::new()
                .app_data(loaded_scenarios.clone())
                .configure(configure_api),
        )
        .await;

        let created = test::call_service(&app, post_scenario(scenario_body()).to_request()).await;
        assert_eq!(created.status(), 200, "first create must succeed");

        let mut conflicting = scenario_body();
        conflicting["name"] = serde_json::json!("second");
        let rejected = test::call_service(&app, post_scenario(conflicting).to_request()).await;
        assert_eq!(
            rejected.status(),
            409,
            "different content under a taken id must conflict"
        );

        let stored = &loaded_scenarios.read().unwrap().scenarios;
        assert_eq!(
            stored.len(),
            1,
            "the conflicting scenario must not be stored"
        );
        assert_eq!(stored[0].name, "first", "the stored scenario is untouched");
    }

    #[actix_web::test]
    async fn unknown_v1_paths_return_json_404_instead_of_spa_fallback() {
        let loaded_scenarios = Data::new(RwLock::new(LoadedScenarios::new()));
        let app = test::init_service(
            App::new()
                .app_data(loaded_scenarios)
                .configure(configure_api)
                .service(surfpool_studio_ui::serve_studio_static_files),
        )
        .await;

        for path in ["/v1/scenarios/some-id", "/v1/nonexistent"] {
            let request = test::TestRequest::get().uri(path).to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(response.status(), 404, "expected 404 for {path}");
            assert_eq!(
                response.headers().get("content-type").unwrap(),
                "application/json",
                "expected JSON body for {path}"
            );
        }

        let request = test::TestRequest::post()
            .uri("/v1/nonexistent")
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(
            response.status(),
            404,
            "the guard must catch non-GET methods too"
        );
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json",
        );

        let request = test::TestRequest::get().uri("/v1/scenarios").to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(
            response.status(),
            200,
            "registered endpoints must keep working"
        );
    }
}
