use std::path::Path;

pub const BUCEPHALUS_CLOUD_USER_TOKEN_ENV: &str = "BUCEPHALUS_CLOUD_USER_TOKEN";

pub(crate) fn user_auth_hint(message: &str, sent_token: bool, token_path: Option<&Path>) -> String {
    let token_source = if sent_token {
        "The CLI did send a user bearer token, so the token may be expired, malformed, or for the wrong Cloud API audience."
    } else {
        "The CLI did not find a user bearer token before making this request."
    };
    let token_file_hint = token_path
        .map(|path| format!("  - or write an access token to {}", path.display()))
        .unwrap_or_else(|| {
            "  - or write an access token to <BUCEPHALUS_HOME>/auth/cloud_user_token".to_string()
        });

    format!(
        "{message}\n\nCloud auth required.\n{token_source}\nAuthenticate with one of:\n  - buc login\n  - export {BUCEPHALUS_CLOUD_USER_TOKEN_ENV}=<oauth-access-token>\n{token_file_hint}\n\nThen verify local auth state with: buc auth status\nThen verify hosted connectivity with: buc health"
    )
}
