use crate::library::store::Store;

/// Resolve a numeric Steam app id to a game title, caching hits in the store.
/// Returns `None` if the lookup fails. ID extraction is the script's job.
pub fn resolve(store: &Store, app_id: i64) -> Option<String> {
    let id = app_id.to_string();
    if let Ok(Some(name)) = store.steam_get(&id) {
        return Some(name);
    }
    let name = fetch(&id)?;
    let _ = store.steam_put(&id, &name);
    Some(name)
}

fn fetch(id: &str) -> Option<String> {
    let url = format!("https://store.steampowered.com/api/appdetails?appids={id}&filters=basic");
    let body = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(6))
        .call()
        .ok()?
        .into_string()
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v.get(id)?
        .get("data")?
        .get("name")?
        .as_str()
        .map(str::to_owned)
}
