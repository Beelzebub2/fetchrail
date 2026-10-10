use serde::Serialize;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Feature {
    pub available: bool,
    pub reason: String,
}
impl Feature {
    pub fn new(available: bool, reason: impl Into<String>) -> Self {
        Self {
            available,
            reason: reason.into(),
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub name: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub os: &'static str,
    pub startup: Feature,
    pub tray: Feature,
    pub shutdown: Feature,
    pub force_shutdown: Feature,
    pub disconnect: Feature,
    pub connections: Vec<Connection>,
    pub update_owner: String,
    pub browser_integration: Feature,
}
