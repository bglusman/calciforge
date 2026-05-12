use url::Url;

pub(crate) fn is_trusted_secret_helper_url(url: &Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => url
            .host_str()
            .map(|host| matches!(host, "localhost" | "127.0.0.1" | "::1"))
            .unwrap_or(false),
        _ => false,
    }
}

pub(crate) fn render_central_secret_helper_wrapper(
    base_url: &str,
    api_key: Option<&str>,
    agent_id: &str,
) -> String {
    let token_line = api_key
        .map(|token| {
            format!(
                "export CALCIFORGE_SECRETS_TOKEN={}\n",
                shell_quote(token.trim())
            )
        })
        .unwrap_or_default();
    format!(
        "#!/bin/sh\n\
         # Managed by calciforge install. This wrapper talks to the central Calciforge secret store.\n\
         export CALCIFORGE_AGENT_ID={}\n\
         export CALCIFORGE_SECRETS_BASE_URL={}\n\
         {}\
         exec \"$HOME/.local/libexec/calciforge/calciforge-secrets-bin\" \"$@\"\n",
        shell_quote(agent_id.trim()),
        shell_quote(base_url.trim().trim_end_matches('/')),
        token_line
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn central_secret_helper_url_requires_https_or_loopback_http() {
        assert!(is_trusted_secret_helper_url(
            &Url::parse("https://calciforge.example:8080").unwrap()
        ));
        assert!(is_trusted_secret_helper_url(
            &Url::parse("http://127.0.0.1:8080").unwrap()
        ));
        assert!(!is_trusted_secret_helper_url(
            &Url::parse("http://calciforge.example:8080").unwrap()
        ));
    }

    #[test]
    fn central_secret_helper_wrapper_points_to_calciforge_api() {
        let wrapper = render_central_secret_helper_wrapper(
            "https://calciforge.example:8080/",
            Some("secret-token"),
            "research-agent",
        );
        assert!(wrapper.contains("CALCIFORGE_AGENT_ID='research-agent'"));
        assert!(wrapper.contains("CALCIFORGE_SECRETS_BASE_URL='https://calciforge.example:8080'"));
        assert!(wrapper.contains("CALCIFORGE_SECRETS_TOKEN='secret-token'"));
        assert!(wrapper.contains("calciforge-secrets-bin"));
        assert!(
            !wrapper.contains("/control/secrets/set"),
            "wrapper must only configure the helper client; direct secret-control API paths belong in the helper binary"
        );
    }
}
