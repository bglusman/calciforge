use super::gateway::{GatewayConfig, GatewayType};

#[test]
fn wardwright_gateway_type_parses_and_displays() {
    assert_eq!(
        "ward-wright".parse::<GatewayType>().unwrap(),
        GatewayType::Wardwright
    );
    assert_eq!(GatewayType::Wardwright.to_string(), "wardwright");
}

#[test]
fn wardwright_provider_uses_shared_http_core_with_receipt_dashboard_metadata() {
    assert!(GatewayType::Wardwright.uses_openai_compatible_http_core());

    let config = GatewayConfig {
        backend_type: GatewayType::Wardwright,
        ui_url: Some("http://127.0.0.1:8791/admin/runtime".to_string()),
        ..Default::default()
    };
    let info = config.engine_info(GatewayType::Wardwright);

    assert_eq!(info.id, "wardwright");
    assert_eq!(info.display_name, "Wardwright synthetic model gateway");
    assert_eq!(
        info.ui_url.as_deref(),
        Some("http://127.0.0.1:8791/admin/runtime")
    );
    assert!(info.capabilities.openai_chat_completions);
    assert!(info.capabilities.model_listing);
    assert!(
        !info.capabilities.config_validation,
        "Calciforge does not call a Wardwright validation API yet"
    );
    assert!(
        info.observability
            .iter()
            .any(|capability| capability.display_name.contains("receipt"))
    );
}
