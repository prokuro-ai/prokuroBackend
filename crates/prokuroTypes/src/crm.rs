use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrmProviderId {
    Hubspot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrmStatus {
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<CrmProviderId>,
}

/// A company/account record in the customer's CRM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmAccount {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrmAccountSearchResponse {
    pub provider: CrmProviderId,
    pub accounts: Vec<CrmAccount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrmSyncRequest {
    /// CRM company id the BOM risk summary should be logged against.
    pub account_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrmSyncResponse {
    pub provider: CrmProviderId,
    pub account_id: String,
    /// Id of the activity record created in the CRM.
    pub note_id: String,
    pub synced_at: String,
    /// Exact text written to the CRM, so the UI can show what was logged.
    pub body: String,
}
