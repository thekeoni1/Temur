/// The product's credential is provided BY PATH via APP_SECRET_FILE (set by the
/// appsvc launch script). The value is read at startup, used only in the
/// x-api-key request header, and must never be logged, echoed, or placed in
/// argv/env. This binary deliberately does NOT read ANTHROPIC_API_KEY, so the
/// build agent's own auth can never cross-contaminate the product's.
#[allow(dead_code)] // wired to the provider in M2
pub fn load_api_key() -> Result<String, crate::error::Error> {
    let path = std::env::var_os("APP_SECRET_FILE")
        .ok_or_else(|| crate::error::Error::Secret("APP_SECRET_FILE is not set".into()))?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| crate::error::Error::Secret(format!("cannot read credential file: {e}")))?;
    let key = raw.trim();
    if key.is_empty() {
        return Err(crate::error::Error::Secret("credential file is empty".into()));
    }
    Ok(key.to_string())
}
