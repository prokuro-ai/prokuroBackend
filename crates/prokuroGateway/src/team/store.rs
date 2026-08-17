use std::collections::HashMap;

use aws_sdk_dynamodb::types::AttributeValue;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use crate::auth::{AuthUser, TeamRole};
use prokuro_types::purchasing::BillingPlan;

const INVITE_TTL_DAYS: i64 = 7;

#[derive(Debug, thiserror::Error)]
pub enum TeamError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("not found")]
    NotFound,
    #[error("invite expired")]
    Expired,
    #[error("invite email does not match signed-in user")]
    EmailMismatch,
    #[error("{0}")]
    Store(String),
}

#[derive(Debug, Clone)]
pub struct MemberRecord {
    pub user_id: String,
    pub account_id: String,
    pub email: Option<String>,
    pub role: TeamRole,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct InviteRecord {
    pub id: String,
    pub account_id: String,
    pub email: String,
    pub role: TeamRole,
    pub invited_by: String,
    pub expires_at: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct TeamSnapshot {
    pub members: Vec<MemberRecord>,
    pub invites: Vec<InviteRecord>,
}

struct MemoryState {
    users: HashMap<String, MemberRecord>,
    members: HashMap<String, HashMap<String, MemberRecord>>,
    invites: HashMap<String, InviteRecord>,
    plans: HashMap<String, BillingPlan>,
}

#[allow(clippy::large_enum_variant)]
enum StoreMode {
    Memory(RwLock<MemoryState>),
    Dynamo {
        client: aws_sdk_dynamodb::Client,
        table: String,
    },
}

pub struct TeamStore {
    mode: StoreMode,
}

impl TeamStore {
    pub fn memory() -> Self {
        Self {
            mode: StoreMode::Memory(RwLock::new(MemoryState {
                users: HashMap::new(),
                members: HashMap::new(),
                invites: HashMap::new(),
                plans: HashMap::new(),
            })),
        }
    }

    pub async fn from_env() -> Self {
        let table = std::env::var("MEMBERS_TABLE").ok().filter(|v| !v.is_empty());
        if let Some(table) = table {
            let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
            return Self {
                mode: StoreMode::Dynamo {
                    client: aws_sdk_dynamodb::Client::new(&config),
                    table,
                },
            };
        }
        Self::memory()
    }

    pub async fn set_plan_override(&self, account_id: &str, plan: BillingPlan) {
        if let StoreMode::Memory(state) = &self.mode {
            state
                .write()
                .await
                .plans
                .insert(account_id.to_string(), plan);
        }
    }

    pub async fn plan_override(&self, account_id: &str) -> Option<BillingPlan> {
        match &self.mode {
            StoreMode::Memory(state) => state.read().await.plans.get(account_id).copied(),
            StoreMode::Dynamo { .. } => None,
        }
    }

    pub async fn resolve_membership(&self, user: &mut AuthUser) -> Result<(), String> {
        if let Some(member) = self.get_user_membership(&user.user_id).await? {
            user.account_id = member.account_id;
            user.role = member.role;
            if user.email.is_none() {
                user.email = member.email;
            }
            return Ok(());
        }

        let now = Utc::now().to_rfc3339();
        let member = MemberRecord {
            user_id: user.user_id.clone(),
            account_id: user.user_id.clone(),
            email: user.email.clone(),
            role: TeamRole::Owner,
            created_at: now,
        };
        self.put_member(&member).await?;
        user.account_id = member.account_id;
        user.role = TeamRole::Owner;
        Ok(())
    }

    pub async fn snapshot(&self, account_id: &str) -> Result<TeamSnapshot, String> {
        let now = Utc::now();
        let mut snapshot = self.load_snapshot(account_id).await?;
        snapshot.invites.retain(|invite| !is_expired(&invite.expires_at, now));
        snapshot
            .members
            .sort_by(|a, b| a.created_at.cmp(&b.created_at));
        snapshot
            .invites
            .sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(snapshot)
    }

    pub async fn seat_usage(&self, account_id: &str) -> Result<u32, String> {
        let snapshot = self.snapshot(account_id).await?;
        Ok((snapshot.members.len() + snapshot.invites.len()) as u32)
    }

    pub async fn create_invite(
        &self,
        account_id: &str,
        email: &str,
        role: TeamRole,
        invited_by: &str,
    ) -> Result<InviteRecord, TeamError> {
        if !role.can_invite_as() {
            return Err(TeamError::Invalid(
                "invite role must be read_only or admin".into(),
            ));
        }
        let email = normalize_email(email)?;
        let snapshot = self
            .snapshot(account_id)
            .await
            .map_err(TeamError::Store)?;
        if snapshot.members.iter().any(|member| {
            member
                .email
                .as_deref()
                .is_some_and(|existing| existing.eq_ignore_ascii_case(&email))
        }) {
            return Err(TeamError::Conflict(
                "that email is already a member of this account".into(),
            ));
        }
        if snapshot
            .invites
            .iter()
            .any(|invite| invite.email == email)
        {
            return Err(TeamError::Conflict(
                "a pending invite already exists for that email".into(),
            ));
        }

        let now = Utc::now();
        let id = uuid::Uuid::new_v4().to_string();
        let invite = InviteRecord {
            id: id.clone(),
            account_id: account_id.to_string(),
            email,
            role,
            invited_by: invited_by.to_string(),
            expires_at: (now + chrono::Duration::days(INVITE_TTL_DAYS)).to_rfc3339(),
            created_at: now.to_rfc3339(),
        };
        self.put_invite(&invite).await.map_err(TeamError::Store)?;
        Ok(invite)
    }

    pub async fn revoke_invite(&self, account_id: &str, invite_id: &str) -> Result<(), TeamError> {
        let invite = self
            .get_invite(invite_id)
            .await
            .map_err(TeamError::Store)?
            .ok_or(TeamError::NotFound)?;
        if invite.account_id != account_id {
            return Err(TeamError::NotFound);
        }
        self.delete_invite(invite_id).await.map_err(TeamError::Store)
    }

    pub async fn remove_member(
        &self,
        account_id: &str,
        user_id: &str,
    ) -> Result<(), TeamError> {
        let member = self
            .get_member(account_id, user_id)
            .await
            .map_err(TeamError::Store)?
            .ok_or(TeamError::NotFound)?;
        if member.role == TeamRole::Owner {
            return Err(TeamError::Forbidden("cannot remove the account owner".into()));
        }
        self.delete_member(account_id, user_id)
            .await
            .map_err(TeamError::Store)
    }

    pub async fn patch_member_role(
        &self,
        account_id: &str,
        user_id: &str,
        role: TeamRole,
    ) -> Result<MemberRecord, TeamError> {
        if role == TeamRole::Owner {
            return Err(TeamError::Invalid("ownership is not transferable".into()));
        }
        if !role.can_invite_as() {
            return Err(TeamError::Invalid("role must be read_only or admin".into()));
        }
        let mut member = self
            .get_member(account_id, user_id)
            .await
            .map_err(TeamError::Store)?
            .ok_or(TeamError::NotFound)?;
        if member.role == TeamRole::Owner {
            return Err(TeamError::Forbidden("cannot change the owner role".into()));
        }
        member.role = role;
        self.put_member(&member).await.map_err(TeamError::Store)?;
        Ok(member)
    }

    #[cfg(test)]
    pub async fn insert_expired_invite(
        &self,
        account_id: &str,
        email: &str,
        role: TeamRole,
    ) -> InviteRecord {
        let invite = InviteRecord {
            id: uuid::Uuid::new_v4().to_string(),
            account_id: account_id.to_string(),
            email: email.to_string(),
            role,
            invited_by: account_id.to_string(),
            expires_at: (Utc::now() - chrono::Duration::days(1)).to_rfc3339(),
            created_at: Utc::now().to_rfc3339(),
        };
        self.put_invite(&invite).await.expect("put expired invite");
        invite
    }

    pub async fn accept_invite(
        &self,
        token: &str,
        user: &AuthUser,
    ) -> Result<MemberRecord, TeamError> {
        let invite = self
            .get_invite(token)
            .await
            .map_err(TeamError::Store)?
            .ok_or(TeamError::NotFound)?;
        if is_expired(&invite.expires_at, Utc::now()) {
            let _ = self.delete_invite(&invite.id).await;
            return Err(TeamError::Expired);
        }
        let Some(email) = user.email.as_deref() else {
            return Err(TeamError::Invalid(
                "signed-in user has no email; cannot accept invite".into(),
            ));
        };
        if normalize_email(email)? != invite.email {
            return Err(TeamError::EmailMismatch);
        }

        if let Some(existing) = self
            .get_user_membership(&user.user_id)
            .await
            .map_err(TeamError::Store)?
        {
            if existing.account_id == invite.account_id {
                return Err(TeamError::Conflict(
                    "you already belong to this account".into(),
                ));
            }
            if existing.role == TeamRole::Owner {
                let snapshot = self
                    .snapshot(&existing.account_id)
                    .await
                    .map_err(TeamError::Store)?;
                if snapshot.members.len() > 1 || !snapshot.invites.is_empty() {
                    return Err(TeamError::Conflict(
                        "leave or remove other members from your current account before joining another team".into(),
                    ));
                }
            }
            // Solo owners (and non-owners) leave their previous account before joining.
            self.delete_member(&existing.account_id, &user.user_id)
                .await
                .map_err(TeamError::Store)?;
        }

        let member = MemberRecord {
            user_id: user.user_id.clone(),
            account_id: invite.account_id.clone(),
            email: Some(invite.email.clone()),
            role: invite.role,
            created_at: Utc::now().to_rfc3339(),
        };
        self.put_member(&member).await.map_err(TeamError::Store)?;
        self.delete_invite(&invite.id)
            .await
            .map_err(TeamError::Store)?;
        Ok(member)
    }

    async fn get_user_membership(&self, user_id: &str) -> Result<Option<MemberRecord>, String> {
        match &self.mode {
            StoreMode::Memory(state) => Ok(state.read().await.users.get(user_id).cloned()),
            StoreMode::Dynamo { client, table } => {
                let result = client
                    .get_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("USER#{user_id}")))
                    .key("sk", AttributeValue::S("ACCOUNT".into()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(result.item.and_then(member_from_item))
            }
        }
    }

    async fn get_member(
        &self,
        account_id: &str,
        user_id: &str,
    ) -> Result<Option<MemberRecord>, String> {
        match &self.mode {
            StoreMode::Memory(state) => Ok(state
                .read()
                .await
                .members
                .get(account_id)
                .and_then(|members| members.get(user_id))
                .cloned()),
            StoreMode::Dynamo { client, table } => {
                let result = client
                    .get_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("ACCOUNT#{account_id}")))
                    .key("sk", AttributeValue::S(format!("MEMBER#{user_id}")))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(result.item.and_then(member_from_item))
            }
        }
    }

    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>, String> {
        match &self.mode {
            StoreMode::Memory(state) => Ok(state.read().await.invites.get(invite_id).cloned()),
            StoreMode::Dynamo { client, table } => {
                let token = client
                    .get_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("TOKEN#{invite_id}")))
                    .key("sk", AttributeValue::S("INVITE".into()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                let Some(item) = token.item else {
                    return Ok(None);
                };
                let account_id = item
                    .get("account_id")
                    .and_then(|v| v.as_s().ok())
                    .ok_or_else(|| "invite token missing account_id".to_string())?;
                let result = client
                    .get_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("ACCOUNT#{account_id}")))
                    .key("sk", AttributeValue::S(format!("INVITE#{invite_id}")))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(result.item.and_then(invite_from_item))
            }
        }
    }

    async fn load_snapshot(&self, account_id: &str) -> Result<TeamSnapshot, String> {
        match &self.mode {
            StoreMode::Memory(state) => {
                let state = state.read().await;
                Ok(TeamSnapshot {
                    members: state
                        .members
                        .get(account_id)
                        .map(|members| members.values().cloned().collect())
                        .unwrap_or_default(),
                    invites: state
                        .invites
                        .values()
                        .filter(|invite| invite.account_id == account_id)
                        .cloned()
                        .collect(),
                })
            }
            StoreMode::Dynamo { client, table } => {
                let mut members = Vec::new();
                let mut invites = Vec::new();
                let mut start_key = None;
                loop {
                    let mut query = client
                        .query()
                        .table_name(table)
                        .key_condition_expression("pk = :pk")
                        .expression_attribute_values(
                            ":pk",
                            AttributeValue::S(format!("ACCOUNT#{account_id}")),
                        );
                    if let Some(key) = start_key {
                        query = query.set_exclusive_start_key(Some(key));
                    }
                    let result = query.send().await.map_err(|e| e.to_string())?;
                    for item in result.items.unwrap_or_default() {
                        let sk = item
                            .get("sk")
                            .and_then(|v| v.as_s().ok())
                            .cloned()
                            .unwrap_or_default();
                        if sk.starts_with("MEMBER#") {
                            if let Some(member) = member_from_item(item) {
                                members.push(member);
                            }
                        } else if sk.starts_with("INVITE#") {
                            if let Some(invite) = invite_from_item(item) {
                                invites.push(invite);
                            }
                        }
                    }
                    start_key = result.last_evaluated_key;
                    if start_key.is_none() {
                        break;
                    }
                }
                Ok(TeamSnapshot { members, invites })
            }
        }
    }

    async fn put_member(&self, member: &MemberRecord) -> Result<(), String> {
        match &self.mode {
            StoreMode::Memory(state) => {
                let mut state = state.write().await;
                state
                    .members
                    .entry(member.account_id.clone())
                    .or_default()
                    .insert(member.user_id.clone(), member.clone());
                state.users.insert(member.user_id.clone(), member.clone());
                Ok(())
            }
            StoreMode::Dynamo { client, table } => {
                client
                    .put_item()
                    .table_name(table)
                    .set_item(Some(member_item(member, true)))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                client
                    .put_item()
                    .table_name(table)
                    .set_item(Some(member_item(member, false)))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }

    async fn put_invite(&self, invite: &InviteRecord) -> Result<(), String> {
        match &self.mode {
            StoreMode::Memory(state) => {
                state
                    .write()
                    .await
                    .invites
                    .insert(invite.id.clone(), invite.clone());
                Ok(())
            }
            StoreMode::Dynamo { client, table } => {
                client
                    .put_item()
                    .table_name(table)
                    .set_item(Some(invite_item(invite)))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                let mut token = HashMap::new();
                token.insert(
                    "pk".into(),
                    AttributeValue::S(format!("TOKEN#{}", invite.id)),
                );
                token.insert("sk".into(), AttributeValue::S("INVITE".into()));
                token.insert(
                    "account_id".into(),
                    AttributeValue::S(invite.account_id.clone()),
                );
                token.insert("invite_id".into(), AttributeValue::S(invite.id.clone()));
                if let Some(ttl) = ttl_epoch(&invite.expires_at) {
                    token.insert("ttl".into(), AttributeValue::N(ttl.to_string()));
                }
                client
                    .put_item()
                    .table_name(table)
                    .set_item(Some(token))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }

    async fn delete_invite(&self, invite_id: &str) -> Result<(), String> {
        match &self.mode {
            StoreMode::Memory(state) => {
                state.write().await.invites.remove(invite_id);
                Ok(())
            }
            StoreMode::Dynamo { client, table } => {
                let token = client
                    .get_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("TOKEN#{invite_id}")))
                    .key("sk", AttributeValue::S("INVITE".into()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                if let Some(item) = token.item {
                    if let Some(account_id) = item.get("account_id").and_then(|v| v.as_s().ok()) {
                        client
                            .delete_item()
                            .table_name(table)
                            .key("pk", AttributeValue::S(format!("ACCOUNT#{account_id}")))
                            .key("sk", AttributeValue::S(format!("INVITE#{invite_id}")))
                            .send()
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                }
                client
                    .delete_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("TOKEN#{invite_id}")))
                    .key("sk", AttributeValue::S("INVITE".into()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }

    async fn delete_member(&self, account_id: &str, user_id: &str) -> Result<(), String> {
        match &self.mode {
            StoreMode::Memory(state) => {
                let mut state = state.write().await;
                if let Some(members) = state.members.get_mut(account_id) {
                    members.remove(user_id);
                }
                if state
                    .users
                    .get(user_id)
                    .is_some_and(|member| member.account_id == account_id)
                {
                    state.users.remove(user_id);
                }
                Ok(())
            }
            StoreMode::Dynamo { client, table } => {
                client
                    .delete_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("ACCOUNT#{account_id}")))
                    .key("sk", AttributeValue::S(format!("MEMBER#{user_id}")))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                client
                    .delete_item()
                    .table_name(table)
                    .key("pk", AttributeValue::S(format!("USER#{user_id}")))
                    .key("sk", AttributeValue::S("ACCOUNT".into()))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }
}

fn normalize_email(email: &str) -> Result<String, TeamError> {
    let email = email.trim().to_lowercase();
    if email.len() < 5 || !email.contains('@') || !email.contains('.') {
        return Err(TeamError::Invalid("invalid email".into()));
    }
    Ok(email)
}

fn is_expired(expires_at: &str, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(expires_at)
        .map(|value| value.with_timezone(&Utc) <= now)
        .unwrap_or(true)
}

fn ttl_epoch(expires_at: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(expires_at)
        .ok()
        .map(|value| value.timestamp())
}

fn get_s(item: &HashMap<String, AttributeValue>, key: &str) -> Option<String> {
    item.get(key)
        .and_then(|v| v.as_s().ok())
        .map(|s| s.to_string())
}

fn member_from_item(item: HashMap<String, AttributeValue>) -> Option<MemberRecord> {
    Some(MemberRecord {
        user_id: get_s(&item, "user_id")?,
        account_id: get_s(&item, "account_id")?,
        email: get_s(&item, "email"),
        role: TeamRole::parse(get_s(&item, "role")?.as_str())?,
        created_at: get_s(&item, "created_at").unwrap_or_default(),
    })
}

fn invite_from_item(item: HashMap<String, AttributeValue>) -> Option<InviteRecord> {
    Some(InviteRecord {
        id: get_s(&item, "invite_id")?,
        account_id: get_s(&item, "account_id")?,
        email: get_s(&item, "email")?,
        role: TeamRole::parse(get_s(&item, "role")?.as_str())?,
        invited_by: get_s(&item, "invited_by").unwrap_or_default(),
        expires_at: get_s(&item, "expires_at")?,
        created_at: get_s(&item, "created_at").unwrap_or_default(),
    })
}

fn member_item(member: &MemberRecord, account_row: bool) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::new();
    if account_row {
        item.insert(
            "pk".into(),
            AttributeValue::S(format!("ACCOUNT#{}", member.account_id)),
        );
        item.insert(
            "sk".into(),
            AttributeValue::S(format!("MEMBER#{}", member.user_id)),
        );
    } else {
        item.insert(
            "pk".into(),
            AttributeValue::S(format!("USER#{}", member.user_id)),
        );
        item.insert("sk".into(), AttributeValue::S("ACCOUNT".into()));
    }
    item.insert("user_id".into(), AttributeValue::S(member.user_id.clone()));
    item.insert(
        "account_id".into(),
        AttributeValue::S(member.account_id.clone()),
    );
    item.insert(
        "role".into(),
        AttributeValue::S(member.role.as_str().into()),
    );
    item.insert(
        "created_at".into(),
        AttributeValue::S(member.created_at.clone()),
    );
    if let Some(email) = &member.email {
        item.insert("email".into(), AttributeValue::S(email.clone()));
    }
    item
}

fn invite_item(invite: &InviteRecord) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::new();
    item.insert(
        "pk".into(),
        AttributeValue::S(format!("ACCOUNT#{}", invite.account_id)),
    );
    item.insert(
        "sk".into(),
        AttributeValue::S(format!("INVITE#{}", invite.id)),
    );
    item.insert("invite_id".into(), AttributeValue::S(invite.id.clone()));
    item.insert(
        "account_id".into(),
        AttributeValue::S(invite.account_id.clone()),
    );
    item.insert("email".into(), AttributeValue::S(invite.email.clone()));
    item.insert(
        "role".into(),
        AttributeValue::S(invite.role.as_str().into()),
    );
    item.insert(
        "invited_by".into(),
        AttributeValue::S(invite.invited_by.clone()),
    );
    item.insert(
        "expires_at".into(),
        AttributeValue::S(invite.expires_at.clone()),
    );
    item.insert(
        "created_at".into(),
        AttributeValue::S(invite.created_at.clone()),
    );
    item.insert("status".into(), AttributeValue::S("pending".into()));
    if let Some(ttl) = ttl_epoch(&invite.expires_at) {
        item.insert("ttl".into(), AttributeValue::N(ttl.to_string()));
    }
    item
}
