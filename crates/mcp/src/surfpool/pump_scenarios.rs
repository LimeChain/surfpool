use surfpool_types::{
    CHANGE_TO_DEFAULT_STUDIO_PORT_ONCE_SUPERVISOR_MERGED, DEFAULT_RPC_PORT, Scenario,
};

/// Resolves the surfnet RPC URL a pump preset tool should talk to.
pub(super) fn resolve_surfnet_address(surfnet_address: Option<String>) -> String {
    surfnet_address
        .map(|address| address.trim().to_string())
        .filter(|address| !address.is_empty())
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", DEFAULT_RPC_PORT))
}

/// Stores a built scenario through the studio's generic `POST /v1/scenarios`
/// endpoint and returns the stored scenario id.
pub(super) async fn stage_scenario(scenario: &Scenario) -> Result<String, String> {
    let endpoint = format!(
        "http://127.0.0.1:{}/v1/scenarios",
        CHANGE_TO_DEFAULT_STUDIO_PORT_ONCE_SUPERVISOR_MERGED
    );
    let response = reqwest::Client::new()
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!(scenario))
        .send()
        .await
        .map_err(|error| format!("Failed to store the scenario at {endpoint}: {error}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(if body.is_empty() {
            format!("Storing the scenario failed with HTTP {status}")
        } else {
            format!("Storing the scenario failed: {body}")
        });
    }

    let stored: serde_json::Value = serde_json::from_str(&body)
        .map_err(|error| format!("Invalid scenario store response: {error}"))?;
    Ok(stored
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&scenario.id)
        .to_string())
}

pub(super) fn studio_editor_url(scenario_id: &str) -> String {
    format!(
        "http://127.0.0.1:{}/scenarios?id={}&tab=editor",
        CHANGE_TO_DEFAULT_STUDIO_PORT_ONCE_SUPERVISOR_MERGED, scenario_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surfnet_address_defaults_to_the_local_rpc_port() {
        assert_eq!(
            resolve_surfnet_address(None),
            format!("http://127.0.0.1:{}", DEFAULT_RPC_PORT)
        );
        assert_eq!(
            resolve_surfnet_address(Some("  ".to_string())),
            format!("http://127.0.0.1:{}", DEFAULT_RPC_PORT)
        );
        assert_eq!(
            resolve_surfnet_address(Some("http://127.0.0.1:18899".to_string())),
            "http://127.0.0.1:18899"
        );
    }
}
